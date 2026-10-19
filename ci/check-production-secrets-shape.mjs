// Production Worker secret-shape check for the web frontend's wrangler config.
//
// Offline policy gate (no credentials, no network): the web Worker ships no
// bindings and needs no secrets, so [env.production] must carry only `name`.
// Anything beyond refuses with a pointer to this file, so a config that grows
// a production vars/secrets table is reviewed here first. Names only; values
// are never read or printed.
//
// Usage: node ci/check-production-secrets-shape.mjs [--root <repo-root>]
// (exit 0 ok, 1 refuse). --root exists for the CLI-level suite, which
// spawns the checker against temp trees; the deploy step passes no args
// and the checker resolves its own repo root. Refuses when a
// wrangler.json/jsonc shadows apps/web/wrangler.toml (wrangler prefers
// JSON over TOML, searching parent directories), so an unreviewed JSON
// production config can never slip past the TOML shape check.
// Tested by ci/test-check-production-secrets-shape.mjs (node --test, no install).
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve, sep } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

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
    `apps/web wrangler.toml declares [${label}]: review production secret requirements in ci/check-production-secrets-shape.mjs before promoting`,
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

// Config names wrangler prefers over wrangler.toml, in wrangler's own
// resolution order (wrangler.json, then wrangler.jsonc, then wrangler.toml,
// searching parent directories from the invocation cwd).
const SHADOW_CONFIG_NAMES = ["wrangler.json", "wrangler.jsonc"];

// Mirror wrangler's upward config search (deploy-production.yml runs
// wrangler without --config from apps/web): any JSON config in appDir or
// an ancestor up to and including stopDir shadows the TOML shape check.
// Returns the shadowing path, or null when the TOML is authoritative.
// Paths only; file bodies are never read here.
export function findShadowWranglerConfig(appDir, stopDir) {
  let dir = resolve(appDir);
  const stop = resolve(stopDir);
  while (dir === stop || dir.startsWith(stop + sep)) {
    for (const name of SHADOW_CONFIG_NAMES) {
      const candidate = join(dir, name);
      if (existsSync(candidate)) return candidate;
    }
    if (dir === stop) return null;
    dir = dirname(dir);
  }
  return null;
}

function resolveCliRoot(argv) {
  let root = null;
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === "--root" && i + 1 < argv.length) {
      root = argv[i + 1];
      i += 1;
    } else {
      throw new Error(
        `unknown argument "${argv[i]}": usage: node ci/check-production-secrets-shape.mjs [--root <repo-root>]`,
      );
    }
  }
  return root === null ? fileURLToPath(new URL("..", import.meta.url)) : resolve(root);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const repoRoot = resolveCliRoot(process.argv.slice(2));
    const appDir = join(repoRoot, "apps", "web");
    const shadow = findShadowWranglerConfig(appDir, repoRoot);
    if (shadow) {
      throw new Error(
        `${shadow} shadows apps/web/wrangler.toml: wrangler prefers wrangler.json/jsonc over wrangler.toml, so review JSON production config support in ci/check-production-secrets-shape.mjs before promoting`,
      );
    }
    assertProductionSecretsShape(readFileSync(join(appDir, "wrangler.toml"), "utf8"));
    console.log("Production Worker secret shape ok: no production vars or secret tables");
  } catch (error) {
    // Names only: never print values or file bodies on a failed check.
    console.error(`::error::${error.message}`);
    process.exitCode = 1;
  }
}
