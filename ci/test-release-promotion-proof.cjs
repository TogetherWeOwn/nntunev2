"use strict";

// Tests for the production promotion receipt proof (release lane).
// Run with `node --test ci/test-release-promotion-proof.cjs` (node builtins
// only, no install) and wired into the ci-ok-required `release-proof` job,
// so loosening a proof check or dropping the retry/repair wiring goes red.
// No network, no credentials: only the offline pure functions, the receipt
// archive bounds via the Python helper, and the committed YAML text.

const { describe, it } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const { validateReceipt, artifactName } = require("./release-promotion-proof.cjs");
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
