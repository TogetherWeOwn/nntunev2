#!/usr/bin/env node
"use strict";

// Build the shared CalVer releaser's plan request for a production promote.
// The release job runs this, then plans and publishes vYYYY.MDD.P through
// TogetherWeOwn/.github/actions/calver (policy production-promotion). This
// script decides nothing about the version: it only proves the promotion.
//
// It keeps the repository's stronger receipt check (release-promotion-proof.cjs
// reads promotion.json and checks the deploy job's promotion and smoke steps)
// and hands the verified run, job and artifact identity to the releaser, which
// re-reads them from GitHub. Notes groups come from release-please-config.json
// `changelog-sections`.
//
// Env: RELEASE_SHA (40-hex, on origin/main), GH_REPO, GH_TOKEN, GITHUB_OUTPUT,
// optional PROMOTION_RUN_ID / PROMOTION_RUN_ATTEMPT selectors (untrusted).
// Needs a full-history checkout and Python 3 for bounded receipt reads.

const { execFileSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");
const { verifyPromotion } = require("./release-promotion-proof.cjs");

const WORKFLOW = ".github/workflows/deploy-production.yml";
const JOBS = ["deploy-production"];

function loadSections(file) {
  const sections = JSON.parse(fs.readFileSync(file, "utf8")).packages["."]["changelog-sections"];
  if (!Array.isArray(sections) || !sections.length)
    throw new Error(`${file} has no changelog-sections`);
  return sections.map(({ type, section, hidden }) =>
    hidden ? { type, section, hidden: true } : { type, section },
  );
}

function buildRequest({ sha, receipt, artifact, sections }) {
  const runId = Number(receipt.run_id);
  const attempt = Number(receipt.run_attempt);
  return {
    phase: "plan",
    input: {
      channel: "stable",
      sha,
      // One production run attempt is one release operation. A repair dispatch
      // that selects the same attempt recovers that frozen plan.
      operation: `production-${runId}-${attempt}`,
      policy: "production-promotion",
      evidence: {
        runId,
        attempt,
        workflow: WORKFLOW,
        jobs: JOBS,
        artifactId: artifact.id,
        artifactName: artifact.name,
        artifactDigest: artifact.digest,
      },
      sections,
    },
  };
}

function main() {
  const sha = (process.env.RELEASE_SHA || "").trim().toLowerCase();
  const repo = process.env.GH_REPO;
  if (!/^[0-9a-f]{40}$/.test(sha))
    throw new Error("RELEASE_SHA must be a full 40-character commit SHA");
  if (!repo) throw new Error("GH_REPO is required");
  try {
    execFileSync("git", ["merge-base", "--is-ancestor", sha, "origin/main"]);
  } catch {
    throw new Error(`${sha} is not on origin/main; refusing to release it`);
  }
  const sections = loadSections(path.join(__dirname, "..", "release-please-config.json"));
  const { receipt, artifact } = verifyPromotion({ repo, sha });
  const request = JSON.stringify(buildRequest({ sha, receipt, artifact, sections }));
  const line = `request=${request}\n`;
  if (process.env.GITHUB_OUTPUT) fs.appendFileSync(process.env.GITHUB_OUTPUT, line);
  process.stdout.write(line);
}

module.exports = { buildRequest, loadSections };

if (require.main === module) {
  try {
    main();
  } catch (err) {
    console.error(`::error title=release-request::${err.message}`);
    process.exit(1);
  }
}
