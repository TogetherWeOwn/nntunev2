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
      throw new Error("unknown flag");
    } else if (out.baseUrl === undefined) {
      out.baseUrl = arg;
    } else {
      throw new Error("unexpected argument");
    }
  }
  return out;
}

function assertBaseUrl(raw) {
  if (typeof raw !== "string" || raw === "") {
    throw new Error("base-url is required (an https origin, e.g. https://example.com)");
  }
  let base;
  try {
    base = new URL(raw);
  } catch {
    throw new Error("base-url must be a valid HTTP(S) origin");
  }
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

const JSON_BYTE_LIMIT = 16 * 1024;
const HTML_BYTE_LIMIT = 1024 * 1024;

class ProbeError extends Error {}

// Do not await cancellation: a hostile stream's cancel promise can itself stall.
function cancelBody(body) {
  try {
    void body?.cancel().catch(() => {});
  } catch {
    // Cleanup must not replace the fixed probe diagnostic.
  }
}

async function getText(url, headers, timeoutMs, byteLimit) {
  const controller = new AbortController();
  const expiresAt = performance.now() + timeoutMs;
  const expired = () => controller.signal.aborted || performance.now() >= expiresAt;
  let reader;
  let finished = false;
  let timer;
  const deadline = new Promise((_, reject) => {
    timer = setTimeout(() => {
      reject(new ProbeError("deadline"));
      controller.abort();
    }, timeoutMs);
  });
  const consume = async () => {
    const response = await fetch(url, { redirect: "manual", signal: controller.signal, headers });
    // Also clean up a fetch implementation that resolves after abort.
    if (expired()) {
      cancelBody(response.body);
      throw new ProbeError("deadline");
    }
    reader = response.body?.getReader();
    if (response.status !== 200) {
      const status = Number.isInteger(response.status) && response.status >= 100 && response.status <= 599
        ? response.status : "invalid";
      throw new ProbeError(`status ${status}`);
    }
    const declaredLength = response.headers.get("content-length");
    if (declaredLength !== null && /^\d+$/.test(declaredLength) && Number(declaredLength) > byteLimit) {
      throw new ProbeError("body too large");
    }
    // Copy only checked bytes: subarray views could retain an oversized backing buffer.
    const bytes = new Uint8Array(byteLimit);
    let length = 0;
    if (reader) {
      while (true) {
        const { done, value } = await reader.read();
        if (expired()) throw new ProbeError("deadline");
        if (done) break;
        if (value.byteLength > byteLimit - length) throw new ProbeError("body too large");
        bytes.set(value, length);
        length += value.byteLength;
      }
    }
    finished = true;
    return new TextDecoder().decode(bytes.subarray(0, length));
  };
  try {
    // One race for the whole request, not one retained deadline handler per chunk.
    return await Promise.race([consume(), deadline]);
  } finally {
    clearTimeout(timer);
    if (!finished) {
      if (reader) cancelBody(reader);
      controller.abort();
    }
    reader?.releaseLock();
  }
}

async function getJson(url, headers, timeoutMs) {
  const text = await getText(url, headers, timeoutMs, JSON_BYTE_LIMIT);
  try {
    return JSON.parse(text);
  } catch {
    throw new ProbeError("invalid JSON");
  }
}

function probeDiagnostic(error) {
  // Never inspect arbitrary exception messages, causes, names or response data.
  return error instanceof ProbeError ? error.message : "request failed";
}

export async function smoke(
  baseUrl,
  { timeoutMs = 5_000, log = console.log, versionOverride, expectedSha } = {},
) {
  const base = assertBaseUrl(baseUrl);
  if (!Number.isInteger(timeoutMs) || timeoutMs <= 0 || timeoutMs > 2_147_483_647) {
    throw new Error("timeout must be a positive bounded integer in milliseconds");
  }
  if (versionOverride !== undefined) assertVersionOverride(versionOverride);
  if (expectedSha !== undefined) assertExpectedSha(expectedSha);
  const headers = versionOverride === undefined ? {} : { "Cloudflare-Workers-Version-Overrides": versionOverride };
  let failures = 0;
  const check = (valid, path, category) => {
    if (!valid) {
      failures += 1;
      log(`FAIL ${path} ${category}`);
    }
  };

  for (const route of JSON_ROUTES) {
    try {
      const body = await getJson(new URL(route.path, base), headers, timeoutMs);
      const keys = body !== null && typeof body === "object" && !Array.isArray(body) ? Object.keys(body).sort() : null;
      check(
        keys !== null && JSON.stringify(keys) === JSON.stringify([...route.keys].sort()),
        route.path, "invalid shape",
      );
      if (route.path === "/api/health" && body && typeof body === "object") {
        check(body.ok === true, route.path, "ok not true");
        check(typeof body.version === "string" && body.version.length > 0, route.path, "invalid version type or length");
      }
      if (route.path === "/api/ready" && body && typeof body === "object") {
        check(body.ok === true, route.path, "ok not true");
      }
      if (route.path === "/api/build" && body && typeof body === "object") {
        const sha = body.sha;
        const looksLikeSha = typeof sha === "string" && (SHA_PATTERN.test(sha) || sha === "unknown");
        check(looksLikeSha, route.path, "invalid sha");
        if (expectedSha !== undefined) check(sha === expectedSha, route.path, "sha mismatch");
      }
    } catch (error) {
      failures += 1;
      log(`FAIL ${route.path}: ${probeDiagnostic(error)}`);
    }
  }

  for (const route of HTML_ROUTES) {
    try {
      const body = await getText(new URL(route.path, base), headers, timeoutMs, HTML_BYTE_LIMIT);
      check(route.marker.test(body), route.path, "marker mismatch");
    } catch (error) {
      failures += 1;
      log(`FAIL ${route.path}: ${probeDiagnostic(error)}`);
    }
  }

  if (failures === 0) log(`smoke ok (${JSON_ROUTES.length + HTML_ROUTES.length} routes)`);
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
  } catch {
    console.error("invalid smoke usage");
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
  } catch {
    console.error("invalid smoke usage");
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
