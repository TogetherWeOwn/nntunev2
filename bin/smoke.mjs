#!/usr/bin/env node
// Production smoke checker for the web frontend (caller-owned contract).
//
// Single attempt, fail closed. The shared gradual-promote action retries
// this checker (several attempts, short sleeps); this file never retries
// internally, so one invocation proves one point in time.
//
// Contract flags (required by the shared action):
//   --allow-indexable      accepted; production apex serves indexable
//                          content, so no indexability assertion runs here.
//   --version-override <worker="uuid">
//                          pins every probe to the new Worker version
//                          through the version-override header, so a canary
//                          is tested directly instead of sampled traffic.
// Optional:
//   --expected-sha <40hex>  proves the serving build is the promoted one:
//                          GET /api/build must return exactly this SHA.
//                          An "unknown" build identity never satisfies it.
//
// Probes are GET-only public routes: no auth, no writes, no secrets.

const VERSION_OVERRIDE_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}="[0-9a-f-]{36}"$/;
const SHA_PATTERN = /^[0-9a-f]{40}$/;
const VERSION_ID_PATTERN =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

export function assertVersionOverride(value) {
  if (typeof value !== "string" || !VERSION_OVERRIDE_PATTERN.test(value)) {
    throw new Error('version-override must look like worker-name="12345678-1234-1234-1234-123456789abc"');
  }
  const uuid = value.slice(value.indexOf('="') + 2, -1);
  if (!VERSION_ID_PATTERN.test(uuid)) {
    throw new Error('version-override uuid must be lowercase hex');
  }
  return value;
}

export function assertExpectedSha(value) {
  if (typeof value !== "string" || !SHA_PATTERN.test(value)) {
    throw new Error("expected-sha must be a full 40-char lowercase hex commit");
  }
  return value;
}

const JSON_ROUTES = [
  { path: "/api/health", keys: ["ok", "version"] },
  { path: "/api/ready", keys: ["ok"] },
  { path: "/api/build", keys: ["sha"] },
];

const HTML_ROUTES = [
  { path: "/", marker: /NNTune/ },
  { path: "/signup", marker: /Get started|sign up/i },
  { path: "/pricing", marker: /plan|pricing/i },
  { path: "/login", marker: /Sign in/i },
  { path: "/onboarding/byop", marker: /byop|server|host/i },
];

function parseArgs(argv) {
  const out = {
    baseUrl: undefined,
    allowIndexable: false,
    versionOverride: undefined,
    expectedSha: undefined,
  };
  const rest = [...argv];
  while (rest.length) {
    const arg = rest.shift();
    if (arg === "--allow-indexable") {
      out.allowIndexable = true;
    } else if (arg === "--version-override") {
      const value = rest.shift();
      if (value === undefined) throw new Error("--version-override needs a value");
      out.versionOverride = assertVersionOverride(value);
    } else if (arg.startsWith("--version-override=")) {
      out.versionOverride = assertVersionOverride(arg.slice("--version-override=".length));
    } else if (arg === "--expected-sha") {
      const value = rest.shift();
      if (value === undefined) throw new Error("--expected-sha needs a value");
      out.expectedSha = assertExpectedSha(value);
    } else if (arg.startsWith("--expected-sha=")) {
      out.expectedSha = assertExpectedSha(arg.slice("--expected-sha=".length));
    } else if (arg === "--help" || arg === "-h") {
      return { help: true };
    } else if (arg.startsWith("-")) {
      throw new Error(`unknown flag ${arg}`);
    } else if (out.baseUrl === undefined) {
      out.baseUrl = arg;
    } else {
      throw new Error(`unexpected argument ${arg}`);
    }
  }
  return out;
}

function assertBaseUrl(raw) {
  if (typeof raw !== "string" || raw === "") {
    throw new Error("base-url is required (an https origin, e.g. https://example.com)");
  }
  const base = new URL(raw);
  if (
    !["http:", "https:"].includes(base.protocol) ||
    base.username ||
    base.password ||
    base.pathname !== "/" ||
    base.search ||
    base.hash
  ) {
    throw new Error("base-url must be an HTTP(S) origin without credentials, path, query or fragment");
  }
  return base;
}

async function getJson(url, headers, timeoutMs) {
  const response = await fetch(url, {
    redirect: "manual",
    signal: AbortSignal.timeout(timeoutMs),
    headers,
  });
  const text = await response.text();
  let body;
  try {
    body = JSON.parse(text);
  } catch {
    return { response, body: undefined, raw: text };
  }
  return { response, body, raw: text };
}

async function getText(url, headers, timeoutMs) {
  const response = await fetch(url, {
    redirect: "manual",
    signal: AbortSignal.timeout(timeoutMs),
    headers,
  });
  return { response, body: await response.text() };
}

export async function smoke(
  baseUrl,
  { timeoutMs = 5_000, log = console.log, versionOverride, expectedSha } = {},
) {
  const base = assertBaseUrl(baseUrl);
  if (versionOverride !== undefined) assertVersionOverride(versionOverride);
  if (expectedSha !== undefined) assertExpectedSha(expectedSha);
  const headers = versionOverride === undefined ? {} : { "Cloudflare-Workers-Version-Overrides": versionOverride };
  let failures = 0;
  const check = (valid, expected, actual) => {
    if (!valid) {
      failures += 1;
      log(`FAIL: expected ${expected}; actual ${actual}`);
    }
  };

  for (const route of JSON_ROUTES) {
    try {
      const { response, body } = await getJson(new URL(route.path, base), headers, timeoutMs);
      check(response.status === 200, `${route.path} status 200`, `${route.path} status ${response.status}`);
      const keys = body !== null && typeof body === "object" && !Array.isArray(body) ? Object.keys(body).sort() : null;
      check(
        keys !== null && JSON.stringify(keys) === JSON.stringify([...route.keys].sort()),
        `${route.path} keys ${JSON.stringify(route.keys)}`,
        `${route.path} keys ${JSON.stringify(keys)}`,
      );
      if (route.path === "/api/health" && body && typeof body === "object") {
        check(body.ok === true, "/api/health ok true", `/api/health ok ${JSON.stringify(body.ok)}`);
        check(
          typeof body.version === "string" && body.version.length > 0,
          "/api/health version non-empty string",
          `/api/health version ${JSON.stringify(body.version)}`,
        );
      }
      if (route.path === "/api/ready" && body && typeof body === "object") {
        check(body.ok === true, "/api/ready ok true", `/api/ready ok ${JSON.stringify(body)}`);
      }
      if (route.path === "/api/build" && body && typeof body === "object") {
        const sha = body.sha;
        const looksLikeSha = typeof sha === "string" && (SHA_PATTERN.test(sha) || sha === "unknown");
        check(looksLikeSha, "/api/build sha 40-hex or unknown", `/api/build sha ${JSON.stringify(sha)}`);
        if (expectedSha !== undefined) {
          check(sha === expectedSha, `/api/build sha ${expectedSha}`, `/api/build sha ${JSON.stringify(sha)}`);
        }
      }
    } catch (error) {
      failures += 1;
      log(`FAIL ${route.path}: ${error instanceof Error ? error.message : String(error)}`);
    }
  }

  for (const route of HTML_ROUTES) {
    try {
      const { response, body } = await getText(new URL(route.path, base), headers, timeoutMs);
      check(response.status === 200, `${route.path} status 200`, `${route.path} status ${response.status}`);
      check(
        route.marker.test(body),
        `${route.path} body matches ${route.marker}`,
        `${route.path} body mismatch (${body.length} chars)`,
      );
    } catch (error) {
      failures += 1;
      log(`FAIL ${route.path}: ${error instanceof Error ? error.message : String(error)}`);
    }
  }

  if (failures === 0) log(`smoke ok: ${base.origin} (${JSON_ROUTES.length + HTML_ROUTES.length} routes)`);
  return failures;
}

function printHelp() {
  console.log(
    [
      "usage: node bin/smoke.mjs <base-url> [--allow-indexable] [--version-override worker=\"uuid\"] [--expected-sha <40hex>]",
      "",
      "Single-attempt production smoke for the web frontend. Exits 0 when every",
      "public probe passes, 1 when any probe fails, 2 on usage errors.",
    ].join("\n"),
  );
}

async function main() {
  let args;
  try {
    args = parseArgs(process.argv.slice(2));
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    printHelp();
    process.exitCode = 2;
    return;
  }
  if (args.help) {
    printHelp();
    return;
  }
  try {
    assertBaseUrl(args.baseUrl);
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    printHelp();
    process.exitCode = 2;
    return;
  }
  const failures = await smoke(args.baseUrl, {
    versionOverride: args.versionOverride,
    expectedSha: args.expectedSha,
  });
  process.exitCode = failures === 0 ? 0 : 1;
}

const invokedDirectly =
  process.argv[1] !== undefined &&
  import.meta.url === (await import("node:url").then((m) => m.pathToFileURL(process.argv[1]).href).catch(() => ""));
if (invokedDirectly) {
  await main();
}
