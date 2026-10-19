// Choose the commit deploy-production promotes when no sha input is given:
// the newest commit whose post-deploy staging evidence passed.
//
// deploy.yml push runs carry the deployed commit as their head_sha (no
// run-name: push runs join on head_sha directly). The newest completed
// success run on main whose full-journey evidence passes decides; a later
// unfinished, failed, or skipped run for the same commit is not outvoted
// by an older pass.
import { appendFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { assertJobsEvidence } from "./production-deploy-gate.mjs";

const shaPattern = /^[0-9a-f]{40}$/;

export async function resolvePromotionSha({ repository, token, fetchImpl = fetch, pages = 5 }) {
  async function get(path) {
    const response = await fetchImpl(`https://api.github.com${path}`, {
      headers: {
        Authorization: `Bearer ${token}`,
        Accept: "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
      },
      redirect: "error",
      signal: AbortSignal.timeout(10_000),
    });
    if (!response.ok) throw new Error(`Promotion lookup failed (HTTP ${response.status})`);
    return response.json();
  }
  const decided = new Set();
  for (let page = 1; page <= pages; page += 1) {
    const query = new URLSearchParams({
      branch: "main",
      event: "push",
      per_page: "20",
      page: String(page),
    });
    const { workflow_runs: runs = [] } = await get(
      `/repos/${repository}/actions/workflows/deploy.yml/runs?${query}`,
    );
    if (!runs.length) break;
    for (const run of runs) {
      const sha = run.head_sha;
      if (typeof sha !== "string" || !shaPattern.test(sha)) continue;
      if (decided.has(sha)) continue;
      decided.add(sha);
      if (run.status !== "completed" || run.conclusion !== "success") continue;
      if (
        run.head_branch !== "main" ||
        run.path !== ".github/workflows/deploy.yml" ||
        run.event !== "push"
      ) {
        continue;
      }
      // Same bar as the gate: the full journey for this run must have
      // passed, never skipped. A skipped journey here is ineligible (falls
      // through to an older eligible SHA or null), never a refusal.
      try {
        const { jobs = [] } = await get(`/repos/${repository}/actions/runs/${run.id}/jobs?per_page=100`);
        assertJobsEvidence(jobs, run.id);
      } catch {
        continue;
      }
      return sha;
    }
  }
  return null;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const input = (process.env.INPUT_SHA ?? "").replace(/\s+/g, "").toLowerCase();
    const sha =
      input ||
      (await resolvePromotionSha({
        repository: process.env.GITHUB_REPOSITORY,
        token: process.env.GITHUB_TOKEN,
      }));
    if (!shaPattern.test(sha ?? "")) {
      throw new Error(
        "No commit to promote: set the sha input, or wait for a deploy whose full journey passed",
      );
    }
    console.log(
      input ? `Deploying the requested commit ${sha}` : `Promoting ${sha}: its staging evidence passed`,
    );
    appendFileSync(process.env.GITHUB_OUTPUT, `sha=${sha}\n`);
  } catch (error) {
    console.error(`::error::${error.message}`);
    process.exitCode = 1;
  }
}
