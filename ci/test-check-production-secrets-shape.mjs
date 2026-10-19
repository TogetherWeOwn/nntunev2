// Tests for the production Worker secret-shape check.
// Run with `node --test ci/test-check-production-secrets-shape.mjs` (node
// builtins only, no install) and wired into the ci-ok-required
// `production-secrets-shape` job, so removing any guard below goes red.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { assertProductionSecretsShape } from "./check-production-secrets-shape.mjs";

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
