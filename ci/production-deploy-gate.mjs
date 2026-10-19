// Production deploy gate: refuses unless every promotion precondition holds.
//
// The pipeline stays disabled until staging end-to-end evidence is real and
// the production hostname is decided. This gate enforces that posture:
//   1. repo variable PRODUCTION_DEPLOY_ENABLED is exactly "true"
//      (absent or any other value refuses, naming the variable);
//   2. no automated-approval escape: PRODUCTION_AUTO_APPROVE=true refuses;
//   3. repo variable PRODUCTION_HOSTNAME is set to a real hostname, never
//      empty and never an obvious placeholder value;
//   4. the run is a workflow_dispatch on main;
//   5. the production Environment exists and carries required reviewers
//      with prevent-self-review plus a main-only deployment branch policy
//      (read through GITHUB_TOKEN; unreadable fails closed);
//   6. exact-SHA staging evidence: the newest push-triggered deploy.yml run
//      on the dispatch SHA completed with success, and its expanded
//      full-journey staging job ("post-deploy-e2e / full journey ...")
//      completed with success with exactly one green "Run full journey"
//      step. A skipped journey or a skipped journey step is missing
//      evidence, never a pass. Once the staging lane publishes
//      ran/skipped/failed journey counts, a counts step must also have
//      run green; until then the green journey step above is the evidence.
//
// Only node builtins plus fetch: the preflight job runs this with no
// install step. Request/response bodies and credentials are never printed.
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

export const SHA40_PATTERN = /^[0-9a-f]{40}$/;

// Values that are never a real production hostname. Compared
// case-insensitively, so an unset variable, a documented example, or a
// "fill in later" marker all refuse with the variable named. Substring
// patterns stay reserved for strings that never occur inside a real
// hostname; bare words (test, todo, tbd, invalid) match only as a full
// dot/hyphen-delimited label, so a real name merely containing them
// (fastest-nntune.com) still passes. Any IPv4 literal (loopback,
// private ranges, or otherwise) refuses: the production target is a DNS
// name, never a literal address.
const PLACEHOLDER_HOSTNAME_SUBSTRINGS = [
  "example",
  "placeholder",
  "changeme",
  "localhost",
  "ubuntu",
];
const PLACEHOLDER_HOSTNAME_LABELS = ["todo", "tbd", "test", "invalid"];
const IPV4_LITERAL = /^\d{1,3}(\.\d{1,3}){3}$/;
const HOSTNAME_SHAPE = /^[a-z0-9]([a-z0-9.-]*[a-z0-9])?$/i;

export function isPlaceholderHostname(value) {
  if (typeof value !== "string") return true;
  const host = value.trim();
  if (host.length === 0 || host.length > 253) return true;
  if (!host.includes(".")) return true;
  if (!HOSTNAME_SHAPE.test(host)) return true;
  if (host.includes("://") || host.includes("/") || host.includes(":") || /\s/.test(host)) {
    return true;
  }
  const lowered = host.toLowerCase();
  if (IPV4_LITERAL.test(lowered)) return true;
  if (PLACEHOLDER_HOSTNAME_SUBSTRINGS.some((pattern) => lowered.includes(pattern))) return true;
  return PLACEHOLDER_HOSTNAME_LABELS.some((label) => lowered.split(/[.-]/).includes(label));
}

export function assertProductionRequest(env) {
  if (env.PRODUCTION_DEPLOY_ENABLED !== "true") {
    throw new Error(
      "Production deployment disabled: repo variable PRODUCTION_DEPLOY_ENABLED must be exactly true",
    );
  }
  if (env.GITHUB_EVENT_NAME !== "workflow_dispatch" || env.GITHUB_REF !== "refs/heads/main") {
    throw new Error("Production deployment requires workflow_dispatch on main");
  }
}

export function assertNoAutoApprove(env) {
  // Human reviewer only in this slice: an automated-approval variable must
  // never silently replace the Environment reviewers below.
  if (env.PRODUCTION_AUTO_APPROVE === "true") {
    throw new Error(
      "PRODUCTION_AUTO_APPROVE is not supported in this slice: promotion requires a human reviewer",
    );
  }
}

export function assertProductionHostname(env) {
  if (isPlaceholderHostname(env.PRODUCTION_HOSTNAME)) {
    throw new Error(
      "Repo variable PRODUCTION_HOSTNAME must name the decided production hostname (never empty or a placeholder)",
    );
  }
  return env.PRODUCTION_HOSTNAME.trim();
}

export function assertProductionCredentials(env) {
  if (!env.CLOUDFLARE_API_TOKEN?.trim() || !env.CLOUDFLARE_ACCOUNT_ID?.trim()) {
    throw new Error(
      "Production-only Cloudflare credentials must be configured in the production Environment",
    );
  }
}

export function assertDeploySha(env) {
  if (!SHA40_PATTERN.test(env.DEPLOY_SHA ?? "")) {
    throw new Error("DEPLOY_SHA must be a full 40-hex commit (resolved by the preflight job)");
  }
  return env.DEPLOY_SHA;
}

export function assertProductionProtection(environment) {
  // Human-reviewer posture: at least one required reviewer, self-review
  // prevented, and deploys limited to main. Any other shape refuses.
  const review = environment.protection_rules?.find((rule) => rule.type === "required_reviewers");
  if (
    environment.name !== "production" ||
    !review?.reviewers?.length ||
    review.prevent_self_review !== true
  ) {
    throw new Error("production Environment must have required reviewers and prevent self-review");
  }
  const policy = environment.deployment_branch_policy;
  const branchRule = environment.protection_rules?.some((rule) => rule.type === "branch_policy");
  if (
    !branchRule ||
    policy?.custom_branch_policies !== true ||
    policy?.protected_branches !== false
  ) {
    throw new Error("production Environment must keep its main-only deployment branch policy");
  }
}

// Caller-side reusable-call job plus the inner full-journey job name. The
// Jobs API expands the reusable workflow's jobs to "<caller> / <called
// job>", so no job is ever named exactly "post-deploy-e2e": caller success
// is derived from the expanded jobs (plus the run conclusion checked by
// the callers) instead of an exact caller job name, and the inner match
// anchors on that prefix and the full-journey label.
const POST_DEPLOY_CALLER_JOB = "post-deploy-e2e";
const FULL_JOURNEY_LABEL = /full journey/i;
// The e2e-staging.yml step that executes the journey. A success job
// conclusion does not prove the journey ran: when staging is unconfigured
// this step is skipped while the job still concludes success.
const RUN_FULL_JOURNEY_STEP = /^run full journey$/i;

export function newestRunOnSha(runs, sha) {
  const matching = (runs ?? []).filter(
    (run) =>
      run.head_sha === sha &&
      run.head_branch === "main" &&
      run.path === ".github/workflows/deploy.yml" &&
      run.event === "push",
  );
  if (matching.length === 0) return null;
  return matching.reduce((a, b) => (b.id > a.id ? b : a));
}

// A staging lane counts step, when present, proves journeys ran instead of
// being skipped green. Matches by name pattern only; the job success below
// stays the evidence until the lane publishes such a step.
const JOURNEY_COUNT_STEP = /journ.*count|ran\/skipped\/failed|ran-journeys|journeys? ran/i;

export function findJourneyCountStep(steps) {
  return (steps ?? []).find((step) => JOURNEY_COUNT_STEP.test(step.name ?? ""));
}

export function assertJobsEvidence(jobs, runId) {
  const journeys = (jobs ?? []).filter(
    (job) => job.name.startsWith(`${POST_DEPLOY_CALLER_JOB} / `) && FULL_JOURNEY_LABEL.test(job.name),
  );
  if (journeys.length !== 1) {
    throw new Error(`deploy.yml run ${runId} has missing or ambiguous full-journey evidence`);
  }
  const [job] = journeys;
  if (job.conclusion === "skipped") {
    throw new Error(
      `deploy.yml run ${runId} skipped the full journey: a skipped journey is not promotion evidence`,
    );
  }
  if (job.conclusion !== "success") {
    throw new Error(`deploy.yml run ${runId} did not pass the full journey`);
  }
  const journeySteps = (job.steps ?? []).filter((step) => RUN_FULL_JOURNEY_STEP.test(step.name ?? ""));
  if (journeySteps.length !== 1 || journeySteps[0].conclusion !== "success") {
    throw new Error(
      `deploy.yml run ${runId} did not execute the full journey: its Run full journey step is not green`,
    );
  }
  const countStep = findJourneyCountStep(job.steps);
  if (countStep && countStep.conclusion !== "success") {
    throw new Error(`deploy.yml run ${runId} did not publish green journey counts`);
  }
  return { runId, journeyJob: job.name };
}

async function apiGet(fetchImpl, token, path) {
  const response = await fetchImpl(`https://api.github.com${path}`, {
    headers: {
      Authorization: `Bearer ${token}`,
      Accept: "application/vnd.github+json",
      "X-GitHub-Api-Version": "2022-11-28",
    },
    redirect: "error",
    signal: AbortSignal.timeout(10_000),
  });
  if (!response.ok) {
    throw new Error(`Staging evidence request failed (HTTP ${response.status})`);
  }
  return response.json();
}

export async function requireStagingEvidence(
  { repository, sha },
  { token, fetchImpl = fetch, pages = 3 } = {},
) {
  if (!token) {
    throw new Error("GitHub Environment protection cannot be verified");
  }
  let latest = null;
  for (let page = 1; page <= pages; page += 1) {
    const query = new URLSearchParams({ branch: "main", event: "push", per_page: "100", page: String(page) });
    const batch =
      (await apiGet(fetchImpl, token, `/repos/${repository}/actions/workflows/deploy.yml/runs?${query}`))
        .workflow_runs ?? [];
    // The API returns newest first; a short page still ends the scan only
    // when no later page can hold a newer match for this SHA.
    const candidate = newestRunOnSha(batch, sha);
    if (candidate && (!latest || candidate.id > latest.id)) latest = candidate;
    if (batch.length < 100) break;
  }
  if (!latest) {
    throw new Error(`Missing or incomplete exact-SHA deploy.yml evidence for ${sha}`);
  }
  if (latest.status !== "completed" || latest.conclusion !== "success") {
    throw new Error(`Latest deploy.yml run on the deployment SHA is not successful`);
  }
  const { jobs = [] } = await apiGet(
    fetchImpl,
    token,
    `/repos/${repository}/actions/runs/${latest.id}/jobs?per_page=100`,
  );
  assertJobsEvidence(jobs, latest.id);
  return { workflow: "deploy.yml", runId: latest.id };
}

// Offline production secret-shape check (no credentials, no network).
// The web Worker ships no bindings and needs no secrets: its committed
// config must carry no production vars/secret tables. Any future
// production var or secret requirement must be reviewed here first
// (declare the required secret NAMES and verify them), so a config that
// grows an env.production table beyond its name refuses before any
// deploy. Parsed TOML-aware with no dependencies: quoted or spaced table
// headers (["env"."production"."vars"], [ env . production . vars ]),
// dotted keys (production.vars.X under [env]), and inline tables
// (env = { production = { vars = ... } }, including multi-line) all
// resolve to the same key paths before the beyond-`name` check. Basic-string
// escapes (\uXXXX, \UXXXXXXXX, \xHH, short escapes) in quoted keys decode
// before comparison, and triple-quoted multi-line strings are skipped whole,
// so neither spelling can hide a production table. \xHH is TOML 1.1: python
// tomllib (1.0) rejects it, but wrangler's parser decodes it, so the gate
// must too. Reads
// names only; values are never read or printed.
// Split a TOML key (table-header inner text or dotted key) into segments,
// honouring single/double-quoted segments and whitespace around dots, so
// ["env"."production"."vars"], [ env . production . vars ], and dotted or
// inline spellings compare equal to their plain form. Unquoted whitespace
// is insignificant; escape sequences inside basic strings decode per TOML
// (a "\U00000070roduction" or "\x70roduction" segment compares equal to
// "production").
// Anything after a backslash that is not a TOML escape is invalid TOML
// (wrangler rejects it), so it stays literal.
const TOML_SHORT_ESCAPES = {
  b: "\b",
  t: "\t",
  n: "\n",
  f: "\f",
  r: "\r",
  '"': '"',
  "\\": "\\",
};

function decodeTomlKeyEscape(text, i) {
  const next = text[i + 1];
  if (next === "u" || next === "U" || next === "x") {
    const width = next === "u" ? 4 : next === "U" ? 8 : 2;
    const hex = text.slice(i + 2, i + 2 + width);
    if (hex.length === width && /^[0-9a-fA-F]+$/.test(hex)) {
      try {
        return { char: String.fromCodePoint(parseInt(hex, 16)), nextIndex: i + 2 + width };
      } catch {
        // Invalid scalar: fall through to literal.
      }
    }
  } else if (TOML_SHORT_ESCAPES[next] !== undefined) {
    return { char: TOML_SHORT_ESCAPES[next], nextIndex: i + 2 };
  }
  return { char: next, nextIndex: i + 2 };
}

function splitTomlKey(text) {
  const segments = [];
  let buf = "";
  let quote = null;
  const push = () => {
    segments.push(buf);
    buf = "";
  };
  for (let i = 0; i < text.length; i += 1) {
    const ch = text[i];
    if (quote) {
      if (ch === "\\" && quote === '"' && i + 1 < text.length) {
        const decoded = decodeTomlKeyEscape(text, i);
        buf += decoded.char;
        i = decoded.nextIndex - 1;
        continue;
      }
      if (ch === quote) {
        quote = null;
        continue;
      }
      buf += ch;
      continue;
    }
    if (ch === '"' || ch === "'") {
      quote = ch;
      continue;
    }
    if (ch === ".") {
      push();
      continue;
    }
    if (/\s/.test(ch)) continue;
    buf += ch;
  }
  push();
  return segments;
}

// Locate a multi-line string closer per TOML: in basic strings a backslash
// escapes the next character, so `\"` never forms a delimiter; literal
// strings have no escapes. Returns -1 when the string continues past this
// line.
function findTripleCloser(line, triple, from) {
  if (triple[0] === "'") return line.indexOf(triple, from);
  for (let i = from; i + 2 < line.length; i += 1) {
    if (line[i] === "\\") {
      i += 1;
      continue;
    }
    if (line[i] === '"' && line[i + 1] === '"' && line[i + 2] === '"') return i;
  }
  return -1;
}

// A multi-line string may end with one or two content quotes immediately
// inside its closing delimiter (the TOML spec's `"""foo""""` form), so the
// closer search lands on the first triple of a 4-5 quote run: consume the
// whole run, otherwise the stray quote flips single-quote state and a
// following production table is swallowed instead of refused.
function consumeTripleCloser(line, closer, quote) {
  let end = closer + 3;
  while (end < closer + 5 && end < line.length && line[end] === quote) end += 1;
  return end;
}

// Strip a `#` comment and drop triple-quoted multi-line strings (values are
// opaque to the shape check). `state.long` persists across lines: it holds
// the `"""`/`'''` opener when a previous line left a string unclosed, so an
// unbalanced quote inside a multi-line value cannot desync later lines.
function stripTomlComment(line, state) {
  let out = "";
  let i = 0;
  if (state.long) {
    const opener = state.long;
    const end = findTripleCloser(line, opener, 0);
    if (end === -1) return "";
    state.long = null;
    i = consumeTripleCloser(line, end, opener[0]);
  }
  let quote = null;
  while (i < line.length) {
    const ch = line[i];
    if (quote) {
      if (ch === "\\" && quote === '"') {
        out += line.slice(i, i + 2);
        i += 2;
        continue;
      }
      out += ch;
      if (ch === quote) quote = null;
      i += 1;
      continue;
    }
    if (ch === "#") break;
    if (ch === '"' || ch === "'") {
      const triple = ch.repeat(3);
      if (line.startsWith(triple, i)) {
        const closer = findTripleCloser(line, triple, i + 3);
        if (closer === -1) {
          state.long = triple;
          break;
        }
        i = consumeTripleCloser(line, closer, ch);
        continue;
      }
      quote = ch;
      out += ch;
      i += 1;
      continue;
    }
    out += ch;
    i += 1;
  }
  return out;
}

// First "=" outside quotes and outside nested tables/arrays.
function splitTomlAssignment(statement) {
  let quote = null;
  let depth = 0;
  for (let i = 0; i < statement.length; i += 1) {
    const ch = statement[i];
    if (quote) {
      if (ch === "\\" && quote === '"') i += 1;
      else if (ch === quote) quote = null;
      continue;
    }
    if (ch === '"' || ch === "'") quote = ch;
    else if (ch === "{" || ch === "[") depth += 1;
    else if (ch === "}" || ch === "]") depth = Math.max(0, depth - 1);
    else if (ch === "=" && depth === 0) return [statement.slice(0, i), statement.slice(i + 1)];
  }
  return null;
}

// Join lines while inside a multi-line inline table/array so every
// statement parses whole. Comment stripping is quote-aware per line.
function joinTomlStatements(text) {
  const statements = [];
  let buf = "";
  let depth = 0;
  let quote = null;
  const stripState = { long: null };
  const scan = (fragment) => {
    for (let i = 0; i < fragment.length; i += 1) {
      const ch = fragment[i];
      if (quote) {
        if (ch === "\\" && quote === '"') i += 1;
        else if (ch === quote) quote = null;
        continue;
      }
      if (ch === '"' || ch === "'") quote = ch;
      else if (ch === "{" || ch === "[") depth += 1;
      else if (ch === "}" || ch === "]") depth = Math.max(0, depth - 1);
    }
  };
  for (const line of String(text).split("\n")) {
    const code = stripTomlComment(line, stripState);
    buf += (buf ? " " : "") + code;
    scan(code);
    if (depth === 0 && !quote && !stripState.long) {
      statements.push(buf);
      buf = "";
    }
  }
  if (buf.trim()) statements.push(buf);
  return statements;
}

function assignTomlPath(root, segments, value) {
  let node = root;
  for (let i = 0; i < segments.length - 1; i += 1) {
    const key = segments[i];
    if (!node[key] || typeof node[key] !== "object" || Array.isArray(node[key])) node[key] = {};
    node = node[key];
  }
  node[segments[segments.length - 1]] = value;
}

// Structural value parse: inline tables become key objects (recursively,
// honouring dotted keys); everything else is opaque. Only key names are
// ever inspected; values are never read or printed.
function parseTomlValue(text) {
  const trimmed = text.trim();
  if (!trimmed.startsWith("{") || !trimmed.endsWith("}")) return { scalar: true };
  const inner = trimmed.slice(1, -1);
  const entries = [];
  let quote = null;
  let depth = 0;
  let start = 0;
  for (let i = 0; i < inner.length; i += 1) {
    const ch = inner[i];
    if (quote) {
      if (ch === "\\" && quote === '"') i += 1;
      else if (ch === quote) quote = null;
      continue;
    }
    if (ch === '"' || ch === "'") quote = ch;
    else if (ch === "{" || ch === "[") depth += 1;
    else if (ch === "}" || ch === "]") depth = Math.max(0, depth - 1);
    else if (ch === "," && depth === 0) {
      entries.push(inner.slice(start, i));
      start = i + 1;
    }
  }
  entries.push(inner.slice(start));
  const table = {};
  for (const entry of entries) {
    if (!entry.trim()) continue;
    const assignment = splitTomlAssignment(entry);
    if (!assignment) continue;
    const segments = splitTomlKey(assignment[0]).filter((s) => s !== "");
    if (segments.length === 0) continue;
    assignTomlPath(table, segments, parseTomlValue(assignment[1]));
  }
  return table;
}

function productionBeyondName(root) {
  const production = root?.env?.production;
  if (!production || typeof production !== "object") return null;
  const nodes = Array.isArray(production) ? production : [production];
  for (const node of nodes) {
    if (!node || typeof node !== "object" || Array.isArray(node)) return "env.production";
    const extra = Object.keys(node).filter((key) => key !== "name");
    if (extra.length > 0) return `env.production.${extra[0]}`;
  }
  return null;
}

function refuseProductionTable(label) {
  throw new Error(
    `apps/web wrangler.toml declares [${label}]: review production secret requirements in ci/production-deploy-gate.mjs before promoting`,
  );
}

export function assertProductionSecretsShape(configText) {
  const root = {};
  let current = [];
  for (const statement of joinTomlStatements(configText)) {
    const trimmed = statement.trim();
    if (!trimmed) continue;
    const header = /^\s*\[{1,2}\s*(.+?)\s*\]{1,2}\s*$/.exec(trimmed);
    if (header) {
      const segments = splitTomlKey(header[1]).filter((s) => s !== "");
      if (segments.length === 0) continue;
      if (
        segments.length > 2 &&
        segments[0] === "env" &&
        segments[1] === "production"
      ) {
        refuseProductionTable(segments.join("."));
      }
      current = segments;
      assignTomlPath(root, segments, {});
      continue;
    }
    const assignment = splitTomlAssignment(trimmed);
    if (!assignment) continue;
    const segments = splitTomlKey(assignment[0]).filter((s) => s !== "");
    if (segments.length === 0) continue;
    const full = [...current, ...segments];
    // The `name` leaf itself (e.g. `name = "..."` under [env.production])
    // is the allowed production name; anything else beyond env.production
    // needs gate review first.
    const beyondName = full.length > 2 && (full[2] !== "name" || full.length > 3);
    if (beyondName && full[0] === "env" && full[1] === "production") {
      refuseProductionTable(full.join("."));
    }
    assignTomlPath(root, full, parseTomlValue(assignment[1]));
    const beyond = productionBeyondName(root);
    if (beyond) refuseProductionTable(beyond);
  }
}

// The checkout must be the promoted SHA itself: the workflow resolves
// DEPLOY_SHA once and checks it out before gating, so a newer main head
// never deploys.
function currentCheckoutSha() {
  return execFileSync("git", ["-c", `safe.directory=${resolve(".")}`, "rev-parse", "HEAD"], {
    encoding: "utf8",
  }).trim();
}

export async function checkProductionGate(env, fetchImpl = fetch, options = {}) {
  let { checkoutSha } = options;
  // Refuse before any API request, and before the protected deploy job starts.
  assertProductionRequest(env);
  assertNoAutoApprove(env);
  const hostname = assertProductionHostname(env);
  const sha = assertDeploySha(env);
  if (!env.GITHUB_TOKEN || !/^[\w.-]+\/[\w.-]+$/.test(env.GITHUB_REPOSITORY ?? "")) {
    throw new Error("GitHub Environment protection cannot be verified");
  }
  const response = await fetchImpl(
    `https://api.github.com/repos/${env.GITHUB_REPOSITORY}/environments/production`,
    {
      headers: {
        Authorization: `Bearer ${env.GITHUB_TOKEN}`,
        Accept: "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
      },
      signal: AbortSignal.timeout(15_000),
    },
  );
  if (!response.ok) {
    throw new Error(`Cannot verify production Environment protection (HTTP ${response.status})`);
  }
  assertProductionProtection(await response.json());
  checkoutSha ??= currentCheckoutSha();
  if (checkoutSha !== sha) {
    throw new Error("Checkout must be the promoted SHA itself");
  }
  const evidence = await requireStagingEvidence(
    { repository: env.GITHUB_REPOSITORY, sha },
    { token: env.GITHUB_TOKEN, fetchImpl },
  );
  return { sha, hostname, evidence };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    if (process.argv[2] === "--credentials") {
      assertProductionCredentials(process.env);
      console.log("Production-only Cloudflare credentials are present");
    } else if (process.argv[2] === "--secrets") {
      assertProductionSecretsShape(
        readFileSync(new URL("../apps/web/wrangler.toml", import.meta.url), "utf8"),
      );
      console.log("Production Worker secret shape ok: no production vars or secret tables");
    } else {
      const result = await checkProductionGate(process.env);
      console.log(
        `Production dispatch, enable flag, reviewer protection, hostname and exact-SHA staging evidence passed: ${result.sha}, deploy.yml run ${result.evidence.runId}, host ${result.hostname}`,
      );
    }
  } catch (error) {
    // Do not print request/response bodies or credentials on a failed call.
    console.error(`::error::${error.message}`);
    process.exitCode = 1;
  }
}
