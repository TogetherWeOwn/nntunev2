"use strict";

// Only GitHub-authenticated producer metadata can make an artifact a receipt.
// Selectors and receipt JSON are untrusted. No deploy, credential fallback or
// unchecked publication is available here; exhausted discovery fails closed.
const { execFileSync } = require("node:child_process");
const path = require("node:path");

const WORKFLOW = ".github/workflows/deploy-production.yml";
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
// Step names pinned to the deploy-production job in this repo: the actual
// promotion, the identity-aware post-promotion smoke, and the receipt steps
// must succeed in order. Renaming a step here or there refuses publication.
const STEPS = [
  "Gradual promotion 5-50-100 with automatic rollback",
  "Verify production serves the promoted build",
  "Record successful production promotion",
  "Upload production promotion proof",
];
// This repo's production lane runs on manual dispatch only (no automatic
// promotion or schedule): only a dispatched run can carry a receipt.
const EVENTS = new Set(["workflow_dispatch"]);
const MAX_ARCHIVE = 64 * 1024;
const MAX_RECEIPT = 4096;
const MAX_ITEMS = 100;
const MAX_CANDIDATES = 20;

function requireProof(condition, reason) {
  if (!condition) throw new Error(`production promotion proof refused: ${reason}`);
}

function id(value) {
  return (Number.isSafeInteger(value) && value > 0) ||
    (typeof value === "string" && /^[1-9][0-9]{0,15}$/.test(value))
    ? String(value)
    : null;
}

function validateReceipt(receipt, { repo, sha, runId, attempt }) {
  requireProof(
    receipt &&
      receipt.schema === 1 &&
      receipt.repository === repo &&
      receipt.workflow === WORKFLOW &&
      receipt.job === "deploy-production" &&
      receipt.sha === sha &&
      receipt.run_id === runId &&
      receipt.run_attempt === attempt &&
      typeof receipt.promoted_version_id === "string" &&
      UUID.test(receipt.promoted_version_id),
    "receipt does not bind the selected SHA, production workflow, version and attempt",
  );
  return receipt;
}

// deploy-production uploads the receipt under this name. The SHA in it is the
// shared releaser's binding between the run and the deployed commit.
function artifactName(sha, runId, attempt) {
  return `production-promotion-${sha}-${runId}-${attempt}`;
}

function verifyPromotion({ repo, sha }) {
  requireProof(
    /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repo) &&
      repo === process.env.GITHUB_REPOSITORY &&
      id(process.env.GITHUB_REPOSITORY_ID),
    "missing repository invocation identity",
  );
  const repositoryId = process.env.GITHUB_REPOSITORY_ID;
  const deadline = Date.now() + 120_000;
  function command(program, args, maxBuffer, input) {
    const remaining = deadline - Date.now();
    requireProof(remaining > 0, "evidence time budget exhausted");
    try {
      return execFileSync(program, args, {
        input,
        maxBuffer,
        timeout: Math.min(15_000, remaining),
        stdio: [input ? "pipe" : "ignore", "pipe", "pipe"],
      });
    } catch (error) {
      const status = /HTTP (\d{3})/.exec(String(error.stderr || ""))?.[1];
      throw new Error(
        `production promotion proof unreadable: ${program} failed${status ? ` (HTTP ${status})` : " or exceeded its bounds"}; not publishing`,
      );
    }
  }
  function api(route) {
    const bytes = command(
      "gh",
      ["api", "--method", "GET", `repos/${repo}/actions/${route}`],
      1024 * 1024,
    );
    try {
      const data = JSON.parse(bytes.toString("utf8"));
      requireProof(data && typeof data === "object" && !Array.isArray(data), "invalid API object");
      return data;
    } catch {
      throw new Error("production promotion proof refused: malformed API response");
    }
  }
  function list(route, key) {
    // One explicitly bounded page. A partial page must never prove uniqueness
    // or absence. Operators can select a specific older run after discovery is
    // exhausted, but a selected run's own jobs/artifacts must remain complete.
    const data = api(`${route}${route.includes("?") ? "&" : "?"}per_page=${MAX_ITEMS}&page=1`);
    requireProof(
      Number.isSafeInteger(data.total_count) &&
        data.total_count >= 0 &&
        data.total_count <= MAX_ITEMS &&
        Array.isArray(data[key]) &&
        data[key].length === data.total_count,
      `incomplete or exhausted ${key} listing`,
    );
    return data[key];
  }
  const workflow = api("workflows/deploy-production.yml");
  requireProof(
    id(workflow.id) && workflow.path === WORKFLOW && workflow.state === "active",
    "wrong workflow identity",
  );

  function validateRun(run, runId, attempt) {
    requireProof(
      id(run.id) === runId &&
        id(run.run_attempt) === attempt &&
        id(run.workflow_id) === id(workflow.id) &&
        run.path === WORKFLOW &&
        run.repository?.full_name === repo &&
        id(run.repository?.id) === repositoryId &&
        run.head_repository?.full_name === repo &&
        id(run.head_repository?.id) === repositoryId &&
        run.head_branch === "main" &&
        /^[0-9a-f]{40}$/.test(run.head_sha) &&
        EVENTS.has(run.event) &&
        ["completed", "in_progress"].includes(run.status),
      "wrong repository, workflow, main producer, run or attempt",
    );
    // The workflow may check out a main tip newer than the selected deployment.
    // It still must itself be a reviewed main ancestor; the receipt binds sha.
    command("git", ["merge-base", "--is-ancestor", run.head_sha, "origin/main"], 4096);
  }

  function fromRun(runId, attempt) {
    const run = api(`runs/${runId}/attempts/${attempt}`);
    validateRun(run, runId, attempt);
    const jobs = list(`runs/${runId}/attempts/${attempt}/jobs`, "jobs");
    const deploys = jobs.filter((job) => job.name === "deploy-production");
    requireProof(deploys.length === 1, "missing or ambiguous deploy-production job");
    const job = deploys[0];
    requireProof(
      id(job.id) &&
        id(job.run_id) === runId &&
        id(job.run_attempt) === attempt &&
        job.status === "completed" &&
        job.conclusion === "success" &&
        Array.isArray(job.steps) &&
        job.steps.length <= MAX_ITEMS,
      "deploy job did not succeed in the selected attempt",
    );
    let previous = 0;
    for (const name of STEPS) {
      const matches = job.steps.filter((step) => step.name === name);
      const step = matches[0];
      requireProof(
        matches.length === 1 &&
          step.status === "completed" &&
          step.conclusion === "success" &&
          Number.isSafeInteger(step.number) &&
          step.number > previous,
        "actual promotion, identity-aware smoke and receipt steps must succeed in order",
      );
      previous = step.number;
    }
    const name = artifactName(sha, runId, attempt);
    const artifacts = list(`runs/${runId}/artifacts`, "artifacts").filter(
      (artifact) => artifact.name === name,
    );
    requireProof(artifacts.length === 1, "missing or ambiguous attempt-bound artifact");
    const artifact = artifacts[0];
    const producer = artifact.workflow_run;
    requireProof(
      id(artifact.id) &&
        artifact.expired === false &&
        Number.isSafeInteger(artifact.size_in_bytes) &&
        artifact.size_in_bytes > 0 &&
        artifact.size_in_bytes <= MAX_ARCHIVE &&
        (!artifact.expires_at || Date.parse(artifact.expires_at) > Date.now()) &&
        typeof artifact.digest === "string" &&
        /^sha256:[0-9a-f]{64}$/.test(artifact.digest) &&
        id(producer?.id) === runId &&
        id(producer?.repository_id) === repositoryId &&
        id(producer?.head_repository_id) === repositoryId &&
        producer?.head_branch === "main" &&
        producer?.head_sha === run.head_sha,
      "expired, oversized or mismatched artifact producer metadata",
    );
    const archive = command(
      "gh",
      ["api", "--method", "GET", `repos/${repo}/actions/artifacts/${id(artifact.id)}/zip`],
      MAX_ARCHIVE,
    );
    // Read a fixed bounded member in memory; never gh run download/unzip or
    // filesystem extraction of untrusted paths. Python's stdlib checks CRC too.
    const bytes = command(
      "python3",
      [path.join(__dirname, "read-promotion-receipt.py")],
      MAX_RECEIPT,
      archive,
    );
    let receipt;
    try {
      receipt = JSON.parse(bytes.toString("utf8"));
    } catch {
      throw new Error("production promotion proof refused: malformed receipt JSON");
    }
    validateReceipt(receipt, { repo, sha, runId, attempt });
    // The shared CalVer releaser re-reads this exact artifact by ID and digest.
    return {
      receipt,
      artifact: { id: Number(artifact.id), name: artifact.name, digest: artifact.digest },
    };
  }

  const hint = process.env.PROMOTION_RUN_ID || "";
  const attemptHint = process.env.PROMOTION_RUN_ATTEMPT || "";
  if (hint || attemptHint) {
    requireProof(
      id(hint) && id(attemptHint),
      "both run and attempt selectors must be positive decimal IDs",
    );
    return fromRun(hint, attemptHint);
  }
  // Do not filter by head_sha: preflight may select an older deployed SHA than
  // github.sha. No global-success filter: downstream publication may have failed.
  const runs = list(`workflows/${id(workflow.id)}/runs?branch=main`, "workflow_runs");
  let candidates = 0;
  for (const run of runs) {
    if (!EVENTS.has(run.event) || run.head_branch !== "main") continue;
    requireProof(
      ++candidates <= MAX_CANDIDATES,
      "candidate budget exhausted; select a run and attempt",
    );
    const runId = id(run.id);
    const attempt = id(run.run_attempt);
    requireProof(runId && attempt, "invalid discovered run identity");
    validateRun(run, runId, attempt);
    // Many successful automatic runs deliberately skip deployment. Only a run
    // with an attempt-bound artifact is a candidate; absence is not authority.
    const artifacts = list(`runs/${runId}/artifacts`, "artifacts");
    // The artifact name carries the deployed SHA, so a promotion of another
    // commit is skipped here rather than refused.
    if (!artifacts.some((artifact) => artifact.name === artifactName(sha, runId, attempt)))
      continue;
    // fromRun still fails closed if the receipt inside disagrees with the name.
    return fromRun(runId, attempt);
  }
  throw new Error(
    "production promotion proof refused: no trusted promotion receipt found; not publishing",
  );
}

module.exports = { verifyPromotion, validateReceipt, artifactName };
