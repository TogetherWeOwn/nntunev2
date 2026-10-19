// Tests for the disabled-by-default production promotion guards.
// Run with `node --test ci/test-production-deploy-gate.mjs` (no install:
// node builtins only) and wired into the ci-ok-required `production-gate`
// job, so removing any guard below goes red.
//
// The Jobs-API fixture mirrors the real payload of deploy.yml run
// 39191921579 (public main push): the reusable post-deploy call expands to
// "post-deploy-e2e / ..." jobs (no job is ever named exactly
// "post-deploy-e2e"), and there the full-journey job concluded success
// while its "Run full journey" step was skipped (staging unconfigured).
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  assertProductionRequest,
  assertNoAutoApprove,
  assertProductionHostname,
  isPlaceholderHostname,
  assertProductionCredentials,
  assertDeploySha,
  assertProductionProtection,
  newestRunOnSha,
  assertJobsEvidence,
  assertProductionSecretsShape,
  requireStagingEvidence,
  checkProductionGate,
} from "./production-deploy-gate.mjs";
import { resolvePromotionSha } from "./resolve-promotion-sha.mjs";
import {
  assertVersionOverride,
  assertExpectedVersionId,
  assertExpectedBuildSha,
  smoke,
} from "../bin/smoke.mjs";

const SHA_A = "a".repeat(40);
const SHA_B = "b".repeat(40);
const UUID = "12345678-1234-1234-1234-123456789abc";

function baseEnv(overrides = {}) {
  return {
    PRODUCTION_DEPLOY_ENABLED: "true",
    PRODUCTION_AUTO_APPROVE: "",
    PRODUCTION_HOSTNAME: "web.nntune.io",
    GITHUB_EVENT_NAME: "workflow_dispatch",
    GITHUB_REF: "refs/heads/main",
    GITHUB_REPOSITORY: "TogetherWeOwn/nntunev2",
    GITHUB_TOKEN: "token",
    DEPLOY_SHA: SHA_A,
    ...overrides,
  };
}

function protectedEnvironment(overrides = {}) {
  return {
    name: "production",
    protection_rules: [
      { type: "required_reviewers", reviewers: [{ id: 1 }], prevent_self_review: true },
      { type: "branch_policy" },
    ],
    deployment_branch_policy: { protected_branches: false, custom_branch_policies: true },
    ...overrides,
  };
}

function jobsPayload({ journeyConclusion = "success", journeyStep = "success", extra = [] } = {}) {
  return [
    { name: "gate", conclusion: "success" },
    { name: "staging-web", conclusion: "success" },
    { name: "staging-api", conclusion: "success" },
    { name: "preview-web", conclusion: "skipped" },
    { name: "post-deploy-e2e / synthetic smoke (health)", conclusion: "success" },
    {
      name: "post-deploy-e2e / full journey (signup → byop → ingest → meter → limits)",
      conclusion: journeyConclusion,
      steps: [
        { name: "Set up job", conclusion: "success" },
        { name: "Check staging configuration", conclusion: "success" },
        { name: "Run full journey", conclusion: journeyStep },
        {
          name: "Publish signup-path evidence summary",
          conclusion: journeyStep === "success" ? "success" : "skipped",
        },
      ],
    },
    ...extra,
  ];
}

describe("assertProductionRequest", () => {
  it("refuses a missing enable flag", () => {
    assert.throws(
      () => assertProductionRequest(baseEnv({ PRODUCTION_DEPLOY_ENABLED: undefined })),
      /^Error: Production deployment disabled: repo variable PRODUCTION_DEPLOY_ENABLED must be exactly true$/,
    );
  });

  it("refuses any non-true enable value", () => {
    for (const value of ["false", "1", "TRUE", " true "]) {
      assert.throws(
        () => assertProductionRequest(baseEnv({ PRODUCTION_DEPLOY_ENABLED: value })),
        /PRODUCTION_DEPLOY_ENABLED must be exactly true/,
      );
    }
  });

  it("refuses non-dispatch events and non-main refs", () => {
    assert.throws(
      () => assertProductionRequest(baseEnv({ GITHUB_EVENT_NAME: "push" })),
      /^Error: Production deployment requires workflow_dispatch on main$/,
    );
    assert.throws(
      () => assertProductionRequest(baseEnv({ GITHUB_REF: "refs/heads/staging" })),
      /^Error: Production deployment requires workflow_dispatch on main$/,
    );
  });

  it("accepts dispatch on main with the exact enable value", () => {
    assertProductionRequest(baseEnv());
  });
});

describe("assertNoAutoApprove", () => {
  it("refuses the automated-approval escape", () => {
    assert.throws(
      () => assertNoAutoApprove(baseEnv({ PRODUCTION_AUTO_APPROVE: "true" })),
      /PRODUCTION_AUTO_APPROVE is not supported/,
    );
  });

  it("accepts an unset escape variable", () => {
    assertNoAutoApprove(baseEnv());
  });
});

describe("production hostname", () => {
  it("refuses empty and placeholder hostnames", () => {
    for (const value of [
      "",
      "   ",
      "example.com",
      "prod-placeholder.example.com",
      "changeme.nntune.io",
      "localhost",
      "web.localhost",
      "todo.nntune.io",
      "test.nntune.io",
      "web.tbd",
      "invalid.nntune.io",
      "203.0.113.10",
      "not a host",
      "no-tld",
    ]) {
      assert.equal(isPlaceholderHostname(value), true, value);
    }
  });

  it("accepts real hostnames that merely contain placeholder words", () => {
    // Bare words (test, todo, tbd, invalid) match only as a full
    // dot/hyphen-delimited label; substrings still pass.
    assert.equal(isPlaceholderHostname("fastest-nntune.com"), false);
    assert.equal(isPlaceholderHostname("web.nntune.io"), false);
  });

  it("assertProductionHostname returns the trimmed host", () => {
    assert.equal(assertProductionHostname(baseEnv()), "web.nntune.io");
    assert.throws(() => assertProductionHostname(baseEnv({ PRODUCTION_HOSTNAME: "" })), /PRODUCTION_HOSTNAME/);
  });
});

describe("credentials and SHA", () => {
  it("refuses missing Cloudflare credentials", () => {
    assert.throws(
      () => assertProductionCredentials({}),
      /Production-only Cloudflare credentials/,
    );
  });

  it("refuses a non-40-hex DEPLOY_SHA", () => {
    assert.throws(() => assertDeploySha({ DEPLOY_SHA: "abc" }), /DEPLOY_SHA must be a full 40-hex/);
    assert.equal(assertDeploySha({ DEPLOY_SHA: SHA_A }), SHA_A);
  });
});

describe("assertProductionProtection", () => {
  it("accepts the protected shape", () => {
    assertProductionProtection(protectedEnvironment());
  });

  it("refuses unprotected Environment shapes", () => {
    assert.throws(() => assertProductionProtection({ ...protectedEnvironment(), name: "staging" }), /required reviewers/);
    assert.throws(
      () =>
        assertProductionProtection({
          ...protectedEnvironment(),
          protection_rules: [{ type: "required_reviewers", reviewers: [], prevent_self_review: true }],
        }),
      /required reviewers/,
    );
    assert.throws(
      () =>
        assertProductionProtection({
          ...protectedEnvironment(),
          protection_rules: [
            { type: "required_reviewers", reviewers: [{ id: 1 }], prevent_self_review: false },
          ],
        }),
      /required reviewers/,
    );
    assert.throws(
      () => assertProductionProtection({ ...protectedEnvironment(), protection_rules: [] }),
      /required reviewers|branch policy/,
    );
    assert.throws(
      () =>
        assertProductionProtection({
          ...protectedEnvironment(),
          deployment_branch_policy: { protected_branches: true, custom_branch_policies: false },
        }),
      /main-only deployment branch policy/,
    );
  });
});

describe("newestRunOnSha", () => {
  it("picks the newest matching push run and ignores the rest", () => {
    const runs = [
      { id: 1, head_sha: SHA_A, head_branch: "main", path: ".github/workflows/deploy.yml", event: "push" },
      { id: 3, head_sha: SHA_A, head_branch: "main", path: ".github/workflows/deploy.yml", event: "push" },
      { id: 9, head_sha: SHA_A, head_branch: "staging", path: ".github/workflows/deploy.yml", event: "push" },
      { id: 7, head_sha: SHA_A, head_branch: "main", path: ".github/workflows/deploy.yml", event: "pull_request" },
    ];
    assert.equal(newestRunOnSha(runs, SHA_A).id, 3);
    assert.equal(newestRunOnSha(runs, SHA_B), null);
  });
});

describe("assertJobsEvidence", () => {
  it("accepts the expanded-jobs payload with a green journey step", () => {
    const result = assertJobsEvidence(jobsPayload(), 39191921579);
    assert.equal(result.runId, 39191921579);
    assert.match(result.journeyJob, /full journey/);
  });

  it("refuses the live skipped-step shape: job success with a skipped journey step", () => {
    assert.throws(
      () => assertJobsEvidence(jobsPayload({ journeyStep: "skipped" }), 39191921579),
      /^Error: deploy\.yml run 39191921579 did not execute the full journey: its Run full journey step is not green$/,
    );
  });

  it("refuses when the journey step list is missing", () => {
    const jobs = jobsPayload().map((job) =>
      job.name.includes("full journey") ? { name: job.name, conclusion: "success" } : job,
    );
    assert.throws(() => assertJobsEvidence(jobs, 1), /did not execute the full journey/);
  });

  it("refuses missing, ambiguous, failed, or job-level-skipped journeys", () => {
    assert.throws(() => assertJobsEvidence([{ name: "gate", conclusion: "success" }], 1), /missing or ambiguous/);
    const doubled = jobsPayload({
      extra: [
        {
          name: "post-deploy-e2e / full journey (retry)",
          conclusion: "success",
          steps: [{ name: "Run full journey", conclusion: "success" }],
        },
      ],
    });
    assert.throws(() => assertJobsEvidence(doubled, 1), /missing or ambiguous/);
    assert.throws(
      () => assertJobsEvidence(jobsPayload({ journeyConclusion: "failure" }), 1),
      /did not pass the full journey/,
    );
    assert.throws(
      () => assertJobsEvidence(jobsPayload({ journeyConclusion: "skipped" }), 1),
      /skipped the full journey/,
    );
  });

  it("does not accept an exact-named caller job alone", () => {
    const jobs = [{ name: "post-deploy-e2e", conclusion: "success" }];
    assert.throws(() => assertJobsEvidence(jobs, 1), /missing or ambiguous/);
  });
});

describe("assertProductionSecretsShape", () => {
  it("accepts the committed config: [env.production] with only a name", () => {
    const config = readFileSync(new URL("../apps/web/wrangler.toml", import.meta.url), "utf8");
    assertProductionSecretsShape(config);
    assertProductionSecretsShape('[env.production]\nname = "nntune-web"\n');
    assertProductionSecretsShape('[env.staging.vars]\nFOO = "x"\n');
  });

  it("refuses production vars in every valid TOML spelling", () => {
    const bypasses = {
      plain: "[env.production.vars]\nFOO = \"x\"\n",
      quoted: '["env"."production"."vars"]\nFOO = "x"\n',
      singleQuoted: "['env'.'production'.'vars']\nFOO = \"x\"\n",
      spaced: "[ env . production . vars ]\nFOO = \"x\"\n",
      dottedUnderEnv: "[env]\nproduction.vars.FOO = \"x\"\n",
      dottedQuoted: "[env]\n\"production\".\"vars\".FOO = \"x\"\n",
      inlineRoot: "env = { production = { vars = { FOO = \"x\" } } }\n",
      inlineUnderEnv: "[env]\nproduction = { vars = { FOO = \"x\" } }\n",
      inlineDotted: "[env.production]\nvars.FOO = \"x\"\n",
      multilineInline: "env = { production = {\n  vars = { FOO = \"x\" }\n} }\n",
      bindings: "[env.production.bindings]\nFOO = \"x\"\n",
    };
    for (const [spelling, config] of Object.entries(bypasses)) {
      assert.throws(() => assertProductionSecretsShape(config), /review production secret requirements/, spelling);
    }
  });

  it("refuses escaped production keys (unicode and TOML 1.1 hex)", () => {
    // \xHH is TOML 1.1: python tomllib rejects it, but wrangler's parser
    // decodes it, so these fixtures are wrangler-oracle-only.
    const bypasses = {
      unicodeHeader: '["env"."\\u0070roduction"."vars"]\nFOO = "x"\n',
      unicodeLongHeader: '["\\U00000065nv"."production"."vars"]\nFOO = "x"\n',
      unicodeInline: 'env = { "\\U00000070roduction" = { vars = { FOO = "x" } } }\n',
      unicodeDotted: '[env]\n"produc\\u0074ion".vars.FOO = "x"\n',
      hexHeader: '["env"."\\x70roduction"."vars"]\nFOO = "x"\n',
      hexMidKey: '["env"."p\\x72oduction"."vars"]\nFOO = "x"\n',
      hexInline: 'env = { "\\x70roduction" = { vars = { FOO = "x" } } }\n',
    };
    for (const [spelling, config] of Object.entries(bypasses)) {
      assert.throws(() => assertProductionSecretsShape(config), /review production secret requirements/, spelling);
    }
    // A decoded escape that is NOT production must not over-refuse.
    assertProductionSecretsShape('["env"."pro\\"duction"]\nname = "x"\n');
  });

  it("skips triple-quoted strings before later production tables", () => {
    const bypasses = {
      basicMultiline: 'description = """foo " bar\nbaz"""\n[env.production.vars]\nFOO = "x"\n',
      literalMultiline: "description = '''a # b\nc'''\n[env.production.vars]\nFOO = \"x\"\n",
      bracesInString: 'meta = """{"a": [1, 2]}"""\n[env.production.vars]\nFOO = "x"\n',
      // One content quote inside the closer (the TOML spec's own form):
      // consuming exactly 3 quotes leaves a stray quote that swallows the
      // following header.
      basicClosePlusOne: '[vars]\nDESC = """foo""""\n[env.production.vars]\nFOO = "x"\n',
      literalClosePlusOne: "[vars]\nDESC = '''foo''''\n[env.production.vars]\nFOO = \"x\"\n",
      basicClosePlusOneMultiline:
        '[vars]\nDESC = """start\nmiddle\nend""""\n[env.production.vars]\nFOO = "x"\n',
      literalClosePlusOneMultiline:
        "[vars]\nDESC = '''start\nmiddle\nend''''\n[env.production.vars]\nFOO = \"x\"\n",
      // An escaped quote must not form a closer: `\"` plus two more quotes
      // is not a delimiter, so the string continues past this line and the
      // lone `"""` line closes it; the header after that is a real table.
      basicEscapedQuoteBeforeCloser:
        "[vars]\nDESC = \"\"\"a\\\"\"\"\nmore = 1\n\"\"\"\n[env.production.vars]\nFOO = \"x\"\n",
      // An unbalanced quote mid-string pins the state.long path: without
      // cross-line tracking it flips quote state and swallows the header.
      basicUnbalancedQuoteMidString:
        '[vars]\nDESC = """start\nmiddle " unbalanced\nend"""\n[env.production.vars]\nFOO = "x"\n',
      literalUnbalancedQuoteMidString:
        "[vars]\nDESC = '''start\nmiddle ' unbalanced\nend'''\n[env.production.vars]\nFOO = \"x\"\n",
    };
    for (const [spelling, config] of Object.entries(bypasses)) {
      assert.throws(() => assertProductionSecretsShape(config), /review production secret requirements/, spelling);
    }
  });
});

describe("resolvePromotionSha", () => {
  function stubFetch({ runs, jobsByRun }) {
    return async (url) => {
      const path = new URL(url).pathname;
      if (path.endsWith("/actions/workflows/deploy.yml/runs")) return { ok: true, json: async () => ({ workflow_runs: runs }) };
      const match = /\/actions\/runs\/(\d+)\/jobs/.exec(path);
      return { ok: true, json: async () => ({ jobs: jobsByRun[match[1]] ?? [] }) };
    };
  }

  function successRun(id, sha) {
    return { id, head_sha: sha, head_branch: "main", path: ".github/workflows/deploy.yml", event: "push", status: "completed", conclusion: "success" };
  }

  it("a later failed run decides its SHA: falls through to the older eligible commit", async () => {
    const fetchImpl = stubFetch({
      runs: [
        { ...successRun(30, SHA_A), status: "completed", conclusion: "failure" },
        successRun(20, SHA_A),
        successRun(10, SHA_B),
      ],
      jobsByRun: { 20: jobsPayload(), 10: jobsPayload() },
    });
    const sha = await resolvePromotionSha({ repository: "o/r", token: "t", fetchImpl });
    assert.equal(sha, SHA_B);
  });

  it("treats a skipped journey as ineligible, never a refusal", async () => {
    const fetchImpl = stubFetch({
      runs: [successRun(20, SHA_A)],
      jobsByRun: { 20: jobsPayload({ journeyStep: "skipped" }) },
    });
    assert.equal(await resolvePromotionSha({ repository: "o/r", token: "t", fetchImpl }), null);
  });

  it("resolves the newest eligible SHA", async () => {
    const fetchImpl = stubFetch({
      runs: [successRun(20, SHA_B), successRun(10, SHA_A)],
      jobsByRun: { 20: jobsPayload(), 10: jobsPayload() },
    });
    assert.equal(await resolvePromotionSha({ repository: "o/r", token: "t", fetchImpl }), SHA_B);
  });
});

describe("staging evidence lookup", () => {
  function stubFetch({ runs, jobs }) {
    return async (url) => {
      const parsed = new URL(url);
      if (parsed.pathname.endsWith("/actions/workflows/deploy.yml/runs")) {
        return { ok: true, json: async () => ({ workflow_runs: runs }) };
      }
      return { ok: true, json: async () => ({ jobs }) };
    };
  }

  it("refuses without a token before any request", async () => {
    await assert.rejects(
      requireStagingEvidence({ repository: "o/r", sha: SHA_A }, { token: "" }),
      /GitHub Environment protection cannot be verified/,
    );
  });

  it("refuses when no deploy run exists for the SHA", async () => {
    const fetchImpl = stubFetch({ runs: [], jobs: [] });
    await assert.rejects(
      requireStagingEvidence({ repository: "o/r", sha: SHA_A }, { token: "t", fetchImpl }),
      /Missing or incomplete exact-SHA deploy\.yml evidence/,
    );
  });
});

describe("smoke identity flags", () => {
  it("pins version and build identity formats", () => {
    assert.equal(assertExpectedVersionId(UUID), UUID);
    assert.throws(() => assertExpectedVersionId(UUID.toUpperCase()), /lowercase UUID/);
    assert.equal(assertExpectedBuildSha(SHA_A), SHA_A);
    assert.throws(() => assertExpectedBuildSha("abc"), /40-hex/);
    assert.equal(assertVersionOverride(`nntune-web="${UUID}"`), `nntune-web="${UUID}"`);
    assert.throws(() => assertVersionOverride("no-equals"), /version-override must look like/);
  });
});

describe("smoke build identity", () => {
  const ORIGIN = "https://web.nntune.io";

  function stubFetch({ sha = SHA_A } = {}) {
    return async (url) => {
      const path = new URL(url).pathname;
      const bodies = {
        "/api/health": JSON.stringify({ ok: true, version: "9.9.9" }),
        "/api/ready": JSON.stringify({ ok: true }),
        "/api/build": JSON.stringify({ sha }),
        "/": "<!doctype html><title>NNTune</title>",
        "/signup": "<!doctype html><title>Get started</title>",
        "/pricing": "<!doctype html><title>pricing plans</title>",
        "/login": "<!doctype html><title>Sign in</title>",
      };
      return {
        status: 200,
        headers: {
          get: (name) =>
            name.toLowerCase() === "content-type"
              ? (path.startsWith("/api/") ? "application/json" : "text/html")
              : null,
        },
        text: async () => bodies[path] ?? "",
      };
    };
  }

  async function runSmoke(fetchImpl, options) {
    const realFetch = globalThis.fetch;
    globalThis.fetch = fetchImpl;
    try {
      return await smoke(ORIGIN, { log: () => {}, ...options });
    } finally {
      globalThis.fetch = realFetch;
    }
  }

  it("returns false when the serving build sha differs from expected", async () => {
    assert.equal(await runSmoke(stubFetch({ sha: SHA_B }), { expectedBuildSha: SHA_A }), false);
  });

  it("returns true on exact build sha match", async () => {
    assert.equal(await runSmoke(stubFetch({ sha: SHA_A }), { expectedBuildSha: SHA_A }), true);
  });

  it("requires a known serving sha under expectedVersionId", async () => {
    assert.equal(await runSmoke(stubFetch({ sha: "unknown" }), { expectedVersionId: UUID }), false);
    assert.equal(await runSmoke(stubFetch({ sha: SHA_A }), { expectedVersionId: UUID }), true);
  });

  it("throws when versionOverride combines with expectedVersionId", async () => {
    await assert.rejects(
      runSmoke(stubFetch(), {
        versionOverride: `nntune-web="${UUID}"`,
        expectedVersionId: UUID,
      }),
      /without --version-override/,
    );
  });
});

describe("checkProductionGate", () => {
  function stubFetch() {
    return async (url) => {
      const parsed = new URL(url);
      if (parsed.pathname.endsWith("/environments/production")) {
        return { ok: true, json: async () => protectedEnvironment() };
      }
      if (parsed.pathname.endsWith("/actions/workflows/deploy.yml/runs")) {
        return {
          ok: true,
          json: async () => ({
            workflow_runs: [
              {
                id: 39191921579,
                head_sha: SHA_A,
                head_branch: "main",
                path: ".github/workflows/deploy.yml",
                event: "push",
                status: "completed",
                conclusion: "success",
              },
            ],
          }),
        };
      }
      return { ok: true, json: async () => ({ jobs: jobsPayload() }) };
    };
  }

  it("refuses a disabled deploy before any API request", async () => {
    const exploding = async () => {
      throw new Error("must not fetch");
    };
    await assert.rejects(
      checkProductionGate(baseEnv({ PRODUCTION_DEPLOY_ENABLED: "false" }), exploding, { checkoutSha: SHA_A }),
      /PRODUCTION_DEPLOY_ENABLED must be exactly true/,
    );
  });

  it("accepts a fully evidenced promotion and refuses a checkout mismatch", async () => {
    const result = await checkProductionGate(baseEnv(), stubFetch(), { checkoutSha: SHA_A });
    assert.equal(result.sha, SHA_A);
    assert.equal(result.evidence.runId, 39191921579);
    await assert.rejects(
      checkProductionGate(baseEnv(), stubFetch(), { checkoutSha: SHA_B }),
      /Checkout must be the promoted SHA itself/,
    );
  });
});
