// Tests for the production Worker secret-shape check.
// Run with `node --test ci/test-check-production-secrets-shape.mjs` (node
// builtins only, no install) and wired into the ci-ok-required
// `production-secrets-shape` job, so removing any guard below goes red.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import {
  assertProductionSecretsShape,
  findShadowWranglerConfig,
} from "./check-production-secrets-shape.mjs";

const checkerCli = fileURLToPath(new URL("./check-production-secrets-shape.mjs", import.meta.url));

// Build a scratch repo-root tree ({ "apps/web/wrangler.toml": ... }) for
// CLI-level tests, removed afterwards. No network, no credentials.
function withTempRoot(files, fn) {
  const root = mkdtempSync(
    join(process.env.PAPERCLIP_RUN_SCRATCH_DIR || tmpdir(), "secrets-shape-cli-"),
  );
  try {
    for (const [rel, content] of Object.entries(files)) {
      const full = join(root, rel);
      mkdirSync(dirname(full), { recursive: true });
      writeFileSync(full, content);
    }
    return fn(root);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function runChecker(root) {
  const result = spawnSync(process.execPath, [checkerCli, "--root", root], { encoding: "utf8" });
  assert.equal(result.error, undefined);
  assert.equal(result.signal, null);
  return result;
}

const CLEAN_TOML = '[env.production]\nname = "nntune-web"\n';
const BAD_TOML = '[env.production.vars]\nFOO = "x"\n';

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

  it("refuses empty production subtables (header alone, no keys)", () => {
    // Keyless on purpose: with no assignment anywhere, only the
    // header-refusal branch can refuse, so deleting it goes red here.
    const bypasses = {
      emptyVarsHeader: "[env.production.vars]\n",
      emptyVarsThenKeylessHeader: "[env.production.vars]\n[other]\n",
      emptyD1ArrayHeader: "[[env.production.d1_databases]]\n",
      emptyQuotedHeader: '["env"."production"."vars"]\n',
    };
    for (const [spelling, config] of Object.entries(bypasses)) {
      assert.throws(() => assertProductionSecretsShape(config), /review production secret requirements/, spelling);
    }
    // A keyless non-production header stays accepted.
    assertProductionSecretsShape("[env.staging.vars]\n[other]\n");
  });

  it("CLI exits 1 on a temp tree whose toml declares production vars", () => {
    // Pins the entrypoint's refusal exit code: deleting
    // `process.exitCode = 1` prints ::error:: but exits 0, going red here.
    withTempRoot({ "apps/web/wrangler.toml": BAD_TOML }, (root) => {
      const result = runChecker(root);
      assert.equal(result.status, 1);
      assert.match(result.stderr, /::error::.*review production secret requirements/);
    });
  });

  it("CLI exits 0 on a clean temp tree", () => {
    withTempRoot({ "apps/web/wrangler.toml": CLEAN_TOML }, (root) => {
      const result = runChecker(root);
      assert.equal(result.status, 0);
      assert.match(result.stdout, /secret shape ok/);
    });
  });

  it("CLI exits 1 when wrangler.json/jsonc shadows the toml", () => {
    const shadowToml = { "apps/web/wrangler.toml": CLEAN_TOML };
    const shadows = {
      appJsonc: "apps/web/wrangler.jsonc",
      appJson: "apps/web/wrangler.json",
      midLevelJson: "apps/wrangler.json",
      repoRootJson: "wrangler.json",
    };
    for (const [where, rel] of Object.entries(shadows)) {
      withTempRoot(
        { ...shadowToml, [rel]: '{"env":{"production":{"vars":{"FOO":"x"}}}}\n' },
        (root) => {
          const result = runChecker(root);
          assert.equal(result.status, 1, where);
          assert.match(result.stderr, /shadows apps\/web\/wrangler\.toml/, where);
        },
      );
    }
    // A JSON config that is NOT on wrangler's upward search path from
    // apps/web must not refuse.
    withTempRoot(
      { ...shadowToml, "docs/wrangler.json": "{}\n" },
      (root) => {
        assert.equal(runChecker(root).status, 0);
      },
    );
  });

  it("shadow search stops at the repo root", () => {
    withTempRoot({ "apps/web/wrangler.toml": CLEAN_TOML }, (root) => {
      const appDir = join(root, "apps", "web");
      assert.equal(findShadowWranglerConfig(appDir, root), null);
      writeFileSync(join(root, "wrangler.jsonc"), "{}\n");
      assert.equal(findShadowWranglerConfig(appDir, root), join(root, "wrangler.jsonc"));
      // Above the root is out of scope even if a shadow sits there.
      assert.equal(findShadowWranglerConfig(appDir, appDir), null);
    });
  });
});
