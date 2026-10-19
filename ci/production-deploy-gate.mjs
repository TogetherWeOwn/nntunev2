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
//      on the dispatch SHA completed with success, and its full-journey
//      staging job completed with success. A skipped full journey is
//      missing evidence, never a pass. Once the staging lane publishes
//      ran/skipped/failed journey counts, a counts step must also have
//      run green; until then the job success above is the evidence.
//
// Only node builtins plus fetch: the preflight job runs this with no
// install step. Request/response bodies and credentials are never printed.
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

export const SHA40_PATTERN = /^[0-9a-f]{40}$/;

// Values that are never a real production hostname. Compared
// case-insensitively against the whole value and as substrings, so an
// unset variable, a documented example, or a "fill in later" marker all
// refuse with the variable named.
const PLACEHOLDER_HOSTNAME_PATTERNS = [
  "example",
  "placeholder",
  "changeme",
  "todo",
  "tbd",
  "localhost",
  "test",
  "invalid",
  "127.",
  "10.",
  "192.168",
  "ubuntu",
];
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
  return PLACEHOLDER_HOSTNAME_PATTERNS.some((pattern) => lowered.includes(pattern));
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
// reusable workflow's jobs surface under "<caller> / <called job>", so the
// inner match anchors on that prefix and the full-journey label.
const POST_DEPLOY_CALLER_JOB = "post-deploy-e2e";
const FULL_JOURNEY_LABEL = /full journey/i;

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
  const caller = (jobs ?? []).filter((job) => job.name === POST_DEPLOY_CALLER_JOB);
  if (caller.length !== 1 || caller[0].conclusion !== "success") {
    throw new Error(
      `deploy.yml run ${runId} has no successful post-deploy staging evidence`,
    );
  }
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
// deploy. Reads names only; values are never read or printed.
export function assertProductionSecretsShape(configText) {
  for (const line of String(configText).split("\n")) {
    const header = /^\s*\[{1,2}\s*([^\]]+?)\s*\]{1,2}\s*(?:#.*)?$/.exec(line);
    if (!header) continue;
    const table = header[1].trim();
    if (table !== "env.production" && table.startsWith("env.production")) {
      throw new Error(
        `apps/web wrangler.toml declares [${table}]: review production secret requirements in ci/production-deploy-gate.mjs before promoting`,
      );
    }
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
