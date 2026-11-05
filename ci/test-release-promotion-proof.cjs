"use strict";

// Tests for the production promotion receipt proof (release lane).
// Run with `node --test ci/test-release-promotion-proof.cjs` (node builtins
// only, no install) and wired into the ci-ok-required `release-proof` job,
// so loosening a proof check or dropping the retry/repair wiring goes red.
// No network, no credentials: only the offline pure functions, the receipt
// archive bounds via the Python helper, and the committed YAML text.

const { describe, it } = require("node:test");
const assert = require("node:assert/strict");
const { execSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const { validateReceipt, artifactName, verifyPromotion } = require("./release-promotion-proof.cjs");
const { buildRequest } = require("./release-request.cjs");

const REPO = "TogetherWeOwn/nntunev2";
const SHA = "634606e7d12199e0ee8f75df5bfd81902f96e5d4";
const RUN_ID = "41538141542";
const ATTEMPT = "1";
const VERSION_ID = "123e4567-e89b-12d3-a456-426614174000";
const WORKFLOW = ".github/workflows/deploy-production.yml";

function goodReceipt(overrides = {}) {
  return {
    schema: 1,
    repository: REPO,
    workflow: WORKFLOW,
    job: "deploy-production",
    sha: SHA,
    promoted_version_id: VERSION_ID,
    run_id: RUN_ID,
    run_attempt: ATTEMPT,
    ...overrides,
  };
}

describe("artifactName", () => {
  it("binds the SHA, run and attempt in the upload name", () => {
    assert.equal(
      artifactName(SHA, RUN_ID, ATTEMPT),
      `production-promotion-${SHA}-${RUN_ID}-${ATTEMPT}`,
    );
  });
});

describe("validateReceipt", () => {
  it("accepts a well-formed receipt and returns it", () => {
    const receipt = goodReceipt();
    assert.equal(
      validateReceipt(receipt, { repo: REPO, sha: SHA, runId: RUN_ID, attempt: ATTEMPT }),
      receipt,
    );
  });

  const refusals = {
    "wrong sha": goodReceipt({ sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }),
    "wrong run id": goodReceipt({ run_id: "99999999999" }),
    "wrong run attempt": goodReceipt({ run_attempt: "9" }),
    "numeric run id": goodReceipt({ run_id: 41538141542 }),
    "wrong repository": goodReceipt({ repository: "TogetherWeOwn/other-repo" }),
    "wrong workflow": goodReceipt({ workflow: ".github/workflows/deploy.yml" }),
    "wrong job": goodReceipt({ job: "staging-api" }),
    "wrong schema": goodReceipt({ schema: 2 }),
    "uppercase version id": goodReceipt({ promoted_version_id: VERSION_ID.toUpperCase() }),
    "truncated version id": goodReceipt({ promoted_version_id: "123e4567-e89b" }),
    "non-uuid version id": goodReceipt({ promoted_version_id: "not-a-version-id" }),
    "missing version id": (() => {
      const receipt = goodReceipt();
      delete receipt.promoted_version_id;
      return receipt;
    })(),
    "missing sha": (() => {
      const receipt = goodReceipt();
      delete receipt.sha;
      return receipt;
    })(),
    "null receipt": null,
  };

  for (const [name, receipt] of Object.entries(refusals)) {
    it(`refuses a receipt with ${name}`, () => {
      assert.throws(
        () => validateReceipt(receipt, { repo: REPO, sha: SHA, runId: RUN_ID, attempt: ATTEMPT }),
        /production promotion proof refused/,
      );
    });
  }

  it("refuses a receipt selected for another repository", () => {
    assert.throws(
      () => validateReceipt(goodReceipt(), { repo: "TogetherWeOwn/other-repo", sha: SHA, runId: RUN_ID, attempt: ATTEMPT }),
      /production promotion proof refused/,
    );
  });
});

describe("buildRequest", () => {
  const sections = [{ type: "feat", section: "Features" }];
  const artifact = {
    id: 987654321,
    name: artifactName(SHA, RUN_ID, ATTEMPT),
    digest: `sha256:${"ab".repeat(32)}`,
  };

  it("builds a plan request bound to the selected run and attempt", () => {
    const request = buildRequest({ sha: SHA, receipt: goodReceipt(), artifact, sections });
    assert.equal(request.phase, "plan");
    assert.equal(request.input.channel, "stable");
    assert.equal(request.input.sha, SHA);
    assert.equal(request.input.operation, `production-${RUN_ID}-${ATTEMPT}`);
    assert.equal(request.input.policy, "production-promotion");
    assert.deepEqual(request.input.evidence, {
      runId: Number(RUN_ID),
      attempt: Number(ATTEMPT),
      workflow: WORKFLOW,
      jobs: ["deploy-production"],
      artifactId: artifact.id,
      artifactName: artifact.name,
      artifactDigest: artifact.digest,
    });
    assert.deepEqual(request.input.sections, sections);
  });

  it("gives a repair dispatch of the same attempt the same operation", () => {
    const first = buildRequest({ sha: SHA, receipt: goodReceipt(), artifact, sections });
    const second = buildRequest({ sha: SHA, receipt: goodReceipt(), artifact, sections });
    assert.equal(second.input.operation, first.input.operation);
  });
});

describe("release retry and repair wiring", () => {
  const root = path.join(__dirname, "..");
  const release = fs.readFileSync(path.join(root, ".github/workflows/release.yml"), "utf8");
  const deployProduction = fs.readFileSync(
    path.join(root, ".github/workflows/deploy-production.yml"),
    "utf8",
  );

  it("retries the CalVer plan before refusing to publish", () => {
    assert.match(release, /Plan the CalVer release \(retry\)/);
    assert.match(release, /Refuse when no CalVer plan succeeded/);
  });

  it("retries a failed publish in repair mode for its own plan", () => {
    assert.match(release, /Tag the deployed commit and publish its release \(retry\)/);
    assert.match(release, /repair mode for its own plan/);
    assert.match(release, /REPAIR === "true"/);
  });

  it("keeps every CalVer call on the pinned shared releaser", () => {
    const uses = release.match(/TogetherWeOwn\/\.github\/actions\/calver@cf983076/g) || [];
    assert.equal(uses.length, 4);
  });

  it("releases only after the production deploy succeeds, for the promoted SHA", () => {
    assert.match(deployProduction, /needs: \[preflight, deploy-production\]/);
    assert.match(deployProduction, /sha: \$\{\{ needs\.preflight\.outputs\.sha \}\}/);
  });

  it("proves the current run, never an older run by default", () => {
    assert.match(deployProduction, /promotion_run_id: \$\{\{ github\.run_id \}\}/);
  });

  it("refuses to record a receipt whose version id is not a lowercase UUID", () => {
    assert.match(deployProduction, /Invalid production promotion receipt identity/);
  });
});

describe("verifyPromotion", () => {
  // Offline harness: a fake `gh` on PATH serves canned GitHub API JSON
  // (and the canned artifact zip) so the run, job and artifact checks run
  // with no network. `git merge-base` stays real: the fake run's head SHA
  // is origin/main itself, always its own ancestor.
  const root = path.join(__dirname, "..");
  const FAKE_REPO_ID = "123456";
  const FAKE_GH_PY = [
    "#!/usr/bin/env python3",
    '"""Fake `gh` for verifyPromotion tests. Serves canned API JSON or the',
    "canned artifact zip from FAKE_* env. Any unexpected route fails.\"\"\"",
    "import io",
    "import json",
    "import os",
    "import sys",
    "import zipfile",
    "def fail(message):",
    "    sys.stderr.write(message + chr(10))",
    "    sys.exit(3)",
    "def respond(obj):",
    "    sys.stdout.write(json.dumps(obj))",
    "    return 0",
    "def build_zip():",
    "    receipt = {",
    '        \"schema\": 1,',
    '        \"repository\": os.environ[\"FAKE_REPO\"],',
    '        \"workflow\": \".github/workflows/deploy-production.yml\",',
    '        \"job\": \"deploy-production\",',
    '        \"sha\": os.environ[\"FAKE_SHA\"],',
    '        \"promoted_version_id\": os.environ[\"FAKE_VERSION_ID\"],',
    '        \"run_id\": os.environ[\"FAKE_RUN_ID\"],',
    '        \"run_attempt\": os.environ[\"FAKE_ATTEMPT\"],',
    "    }",
    "    buf = io.BytesIO()",
    "    with zipfile.ZipFile(buf, \"w\", zipfile.ZIP_DEFLATED) as archive:",
    "        archive.writestr(\"promotion.json\", json.dumps(receipt))",
    "    return buf.getvalue()",
    "def main(argv):",
    "    try:",
    '        route = argv[argv.index(\"GET\") + 1]',
    '        route = route.split("?", 1)[0]',
    "    except (ValueError, IndexError):",
    "        fail(\"unexpected gh argv\")",
    "    repo = os.environ.get(\"FAKE_REPO\", \"\")",
    '    prefix = \"repos/\" + repo + \"/actions/\"',
    "    if not repo or not route.startswith(prefix):",
    "        fail(\"unexpected route \" + route)",
    "    rest = route[len(prefix):]",
    "    scenario = os.environ.get(\"FAKE_SCENARIO\", \"ok\")",
    "    head_sha = os.environ.get(\"FAKE_HEAD_SHA\", \"\")",
    "    run_id = os.environ.get(\"FAKE_RUN_ID\", \"\")",
    "    attempt = os.environ.get(\"FAKE_ATTEMPT\", \"\")",
    "    sha = os.environ.get(\"FAKE_SHA\", \"\")",
    "    repo_id = int(os.environ.get(\"FAKE_REPO_ID\", \"0\") or \"0\")",
    "    if rest == \"workflows/deploy-production.yml\":",
    "        return respond({\"id\": 999001,",
    '            \"path\": \".github/workflows/deploy-production.yml\",',
    '            \"state\": \"active\"})',
    "    if rest == \"runs/\" + run_id + \"/attempts/\" + attempt:",
    "        event = \"push\" if scenario == \"wrong-event\" else \"workflow_dispatch\"",
    "        return respond({\"id\": int(run_id), \"run_attempt\": int(attempt),",
    "            \"workflow_id\": 999001,",
    '            \"path\": \".github/workflows/deploy-production.yml\",',
    "            \"repository\": {\"full_name\": repo, \"id\": repo_id},",
    "            \"head_repository\": {\"full_name\": repo, \"id\": repo_id},",
    '            \"head_branch\": \"main\", \"head_sha\": head_sha,',
    "            \"event\": event, \"status\": \"completed\"})",
    "    if rest == \"runs/\" + run_id + \"/attempts/\" + attempt + \"/jobs\":",
    "        steps = [",
    '            [\"Gradual promotion 5-50-100 with automatic rollback\", 10],',
    '            [\"Verify production serves the promoted build\", 20],',
    '            [\"Record successful production promotion\", 30],',
    '            [\"Upload production promotion proof\", 40],',
    "        ]",
    "        if scenario == \"bad-step-order\":",
    "            steps[1][1], steps[2][1] = steps[2][1], steps[1][1]",
    "        jobs = []",
    "        if scenario != \"missing-job\":",
    "            jobs = [{",
    '                \"name\": \"deploy-production\", \"id\": 777001,',
    "                \"run_id\": int(run_id), \"run_attempt\": int(attempt),",
    '                \"status\": \"completed\", \"conclusion\": \"success\",',
    "                \"steps\": [{\"name\": name, \"status\": \"completed\",",
    '                    \"conclusion\": \"success\", \"number\": number}',
    "                    for name, number in steps],",
    "            }]",
    "        return respond({\"total_count\": len(jobs), \"jobs\": jobs})",
    "    if rest == \"runs/\" + run_id + \"/artifacts\":",
    "        if scenario == \"no-artifact\":",
    "            return respond({\"total_count\": 0, \"artifacts\": []})",
    "        data = build_zip()",
    "        return respond({\"total_count\": 1, \"artifacts\": [{",
    "            \"id\": 555001,",
    "            \"name\": \"production-promotion-\" + sha + \"-\" + run_id + \"-\" + attempt,",
    "            \"expired\": False, \"size_in_bytes\": len(data),",
    '            \"expires_at\": \"2030-01-01T00:00:00Z\",',
    "            \"digest\": \"sha256:\" + \"cd\" * 32,",
    "            \"workflow_run\": {\"id\": int(run_id), \"repository_id\": repo_id,",
    "                \"head_repository_id\": repo_id,",
    '                \"head_branch\": \"main\", \"head_sha\": head_sha},',
    "        }]})",
    "    if rest == \"artifacts/555001/zip\":",
    "        sys.stdout.buffer.write(build_zip())",
    "        return 0",
    "    fail(\"unexpected route \" + route)",
    "if __name__ == \"__main__\":",
    "    sys.exit(main(sys.argv[1:]))",
    "",
  ].join("\n");

  function withFakeGithub(scenario, fn) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "fake-gh-"));
    const headSha = execSync("git rev-parse origin/main", { cwd: root, encoding: "utf8" }).trim();
    assert.match(headSha, /^[0-9a-f]{40}$/);
    const saved = {};
    for (const name of ["PATH", "GITHUB_REPOSITORY", "GITHUB_REPOSITORY_ID",
      "PROMOTION_RUN_ID", "PROMOTION_RUN_ATTEMPT",
      "FAKE_REPO", "FAKE_REPO_ID", "FAKE_HEAD_SHA", "FAKE_RUN_ID",
      "FAKE_ATTEMPT", "FAKE_SHA", "FAKE_VERSION_ID", "FAKE_SCENARIO"]) {
      saved[name] = process.env[name];
    }
    try {
      const fake = path.join(dir, "gh");
      fs.writeFileSync(fake, FAKE_GH_PY);
      fs.chmodSync(fake, 0o755);
      Object.assign(process.env, {
        PATH: dir + path.delimiter + saved.PATH,
        GITHUB_REPOSITORY: REPO,
        GITHUB_REPOSITORY_ID: FAKE_REPO_ID,
        PROMOTION_RUN_ID: RUN_ID,
        PROMOTION_RUN_ATTEMPT: ATTEMPT,
        FAKE_REPO: REPO,
        FAKE_REPO_ID,
        FAKE_HEAD_SHA: headSha,
        FAKE_RUN_ID: RUN_ID,
        FAKE_ATTEMPT: ATTEMPT,
        FAKE_SHA: SHA,
        FAKE_VERSION_ID: VERSION_ID,
        FAKE_SCENARIO: scenario,
      });
      const previous = process.cwd();
      process.chdir(root);
      try {
        return fn();
      } finally {
        process.chdir(previous);
      }
    } finally {
      for (const [name, value] of Object.entries(saved)) {
        if (value === undefined) delete process.env[name];
        else process.env[name] = value;
      }
      fs.rmSync(dir, { recursive: true, force: true });
    }
  }

  it("proves a genuine promotion receipt from run, job and artifact checks", () => {
    const proved = withFakeGithub("ok", () => verifyPromotion({ repo: REPO, sha: SHA }));
    assert.equal(proved.receipt.sha, SHA);
    assert.equal(proved.receipt.run_id, RUN_ID);
    assert.equal(proved.receipt.run_attempt, ATTEMPT);
    assert.equal(proved.artifact.name, artifactName(SHA, RUN_ID, ATTEMPT));
    assert.equal(proved.artifact.id, 555001);
    assert.match(proved.artifact.digest, /^sha256:[0-9a-f]{64}$/);
  });

  it("refuses when no attempt-bound artifact exists", () => {
    assert.throws(
      () => withFakeGithub("no-artifact", () => verifyPromotion({ repo: REPO, sha: SHA })),
      /production promotion proof refused/,
    );
  });

  it("refuses out-of-order proof steps", () => {
    assert.throws(
      () => withFakeGithub("bad-step-order", () => verifyPromotion({ repo: REPO, sha: SHA })),
      /must succeed in order/,
    );
  });

  it("refuses a run that was not a manual dispatch", () => {
    assert.throws(
      () => withFakeGithub("wrong-event", () => verifyPromotion({ repo: REPO, sha: SHA })),
      /wrong repository, workflow, main producer, run or attempt/,
    );
  });

  it("refuses when the deploy job is missing", () => {
    assert.throws(
      () => withFakeGithub("missing-job", () => verifyPromotion({ repo: REPO, sha: SHA })),
      /missing or ambiguous deploy-production job/,
    );
  });
});
