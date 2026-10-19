#!/usr/bin/env node
// Production smoke probe for the web frontend (GET-only public routes).
//
// Probes the serving origin with plain unauthenticated GET requests and
// fails closed (nonzero exit) on the first failing assertion. Exactly one
// attempt per route: the caller retries. Never sends credentials, never
// writes, never prints secret values: failure output names the route plus
// the expected/actual shapes only, never response bodies or header values.
//
// Required flags (the shared gradual-promotion action calls this checker
// with them for canary and apex probes):
//   --version-override worker-name="<lowercase-version-uuid>"
//       Pins canary probes to the candidate Worker version through the
//       version-override header instead of sampled traffic.
//   --allow-indexable
//       The production apex serves indexable HTML; without the flag the
//       probe stays neutral (this frontend carries no indexability header
//       contract, so the flag only records the caller's intent).
//
// Identity flags (post-promotion verification):
//   --expected-version-id <lowercase-uuid>
//       Validated Worker Version UUID format, fail-closed when malformed.
//       Sent with no override header (ordinary routing). This frontend
//       exposes no version-id surface, so the UUID itself is established
//       by the promotion step's resolved promoted-version-id output; the
//       serving-build proof is --expected-build-sha below.
//   --expected-build-sha <40-hex>
//       Requires GET /api/build to report exactly this SHA (the baked
//       build identity, never a source checkout). "unknown" never matches.
import { pathToFileURL } from "node:url";

const SHA40_PATTERN = /^[0-9a-f]{40}$/;
const VERSION_OVERRIDE_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}="[0-9a-f-]{36}"$/;
const VERSION_UUID_PATTERN =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

export function assertVersionOverride(value) {
  if (
    typeof value !== "string" ||
    !VERSION_OVERRIDE_PATTERN.test(value) ||
    !VERSION_UUID_PATTERN.test(value.slice(value.indexOf('="') + 2, -1))
  ) {
    throw new Error(
      'version-override must look like worker-name="12345678-1234-1234-1234-123456789abc"',
    );
  }
  return value;
}

export function assertExpectedVersionId(value) {
  if (typeof value !== "string" || !VERSION_UUID_PATTERN.test(value)) {
    throw new Error(
      'expected-version-id must be a lowercase UUID like "12345678-1234-1234-1234-123456789abc"',
    );
  }
  return value;
}

export function assertExpectedBuildSha(value) {
  if (typeof value !== "string" || !SHA40_PATTERN.test(value)) {
    throw new Error("expected-build-sha must be a full 40-hex commit SHA");
  }
  return value;
}

const routes = [
  { path: "/api/health", statuses: [200], type: "application/json", kind: "health" },
  { path: "/api/ready", statuses: [200], type: "application/json", kind: "ready" },
  { path: "/api/build", statuses: [200], type: "application/json", kind: "build" },
  { path: "/", statuses: [200], type: "text/html", kind: "page", body: /NNTune/ },
  {
    path: "/signup",
    statuses: [200],
    type: "text/html",
    kind: "page",
    body: /Get started|create account/i,
  },
  {
    path: "/pricing",
    statuses: [200],
    type: "text/html",
    kind: "page",
    body: /plan|pricing/i,
  },
  {
    path: "/login",
    statuses: [200],
    type: "text/html",
    kind: "page",
    body: /Sign in/i,
  },
];

function checkJsonShape(kind, data) {
  if (data === null || typeof data !== "object" || Array.isArray(data)) {
    return "valid JSON object";
  }
  if (kind === "health") {
    const keys = Object.keys(data).sort().join(",");
    if (keys !== "ok,version" || data.ok !== true || typeof data.version !== "string") {
      return "exactly { ok: true, version: string }";
    }
    if (data.version.length === 0) {
      return "nonempty version string";
    }
  } else if (kind === "ready") {
    if (data.ok !== true) {
      return "{ ok: true }";
    }
  } else if (kind === "build") {
    if (typeof data.sha !== "string") {
      return "exactly { sha: string }";
    }
  }
  return null;
}

export async function smoke(
  baseUrl,
  {
    timeoutMs = 10_000,
    log = console.log,
    allowIndexable = false,
    versionOverride,
    expectedVersionId,
    expectedBuildSha,
  } = {},
) {
  const base = new URL(baseUrl);
  if (
    !["http:", "https:"].includes(base.protocol) ||
    base.username ||
    base.password ||
    base.pathname !== "/" ||
    base.search ||
    base.hash
  ) {
    throw new Error(
      "base-url must be an HTTP(S) origin without credentials, path, query or fragment",
    );
  }
  if (versionOverride !== undefined) assertVersionOverride(versionOverride);
  if (expectedVersionId !== undefined) assertExpectedVersionId(expectedVersionId);
  if (expectedBuildSha !== undefined) assertExpectedBuildSha(expectedBuildSha);
  // Ordinary-routing identity never pins: an expected version id is proven
  // on unpinned traffic, so combining it with an override is a usage error.
  if (versionOverride !== undefined && expectedVersionId !== undefined) {
    throw new Error("expected-version-id must be probed without --version-override");
  }
  void allowIndexable;
  const overrideHeaders =
    versionOverride === undefined
      ? {}
      : { "Cloudflare-Workers-Version-Overrides": versionOverride };
  let failures = 0;
  for (const route of routes) {
    let routeFailures = 0;
    const check = (valid, expected, actual) => {
      if (!valid) {
        failures += 1;
        routeFailures += 1;
        log(`FAIL ${route.path}: expected ${expected}; actual ${actual}`);
      }
    };
    try {
      const response = await fetch(new URL(route.path, base), {
        method: "GET",
        redirect: "manual",
        signal: AbortSignal.timeout(timeoutMs),
        headers: overrideHeaders,
      });
      check(
        route.statuses.includes(response.status),
        `HTTP ${route.statuses.join(" or ")}`,
        `HTTP ${response.status}`,
      );
      const contentType = response.headers
        .get("content-type")
        ?.split(";")[0]
        .trim()
        .toLowerCase();
      check(contentType === route.type, `Content-Type ${route.type}`, "mismatched content type");
      const body = await response.text();
      if (route.kind === "health" || route.kind === "ready" || route.kind === "build") {
        let data = null;
        try {
          data = JSON.parse(body);
        } catch {
          data = null;
        }
        if (data === null) {
          check(false, "valid JSON object", "invalid JSON");
        } else {
          const shapeError = checkJsonShape(route.kind, data);
          check(shapeError === null, shapeError ?? "valid shape", "unexpected body shape");
        }
        if (route.kind === "build" && data !== null && typeof data === "object") {
          const sha = data.sha;
          if (expectedBuildSha !== undefined) {
            check(
              sha === expectedBuildSha,
              "build sha equal to the promoted commit",
              typeof sha === "string" && SHA40_PATTERN.test(sha)
                ? "a different serving build sha"
                : "missing or unknown build identity",
            );
          } else if (expectedVersionId !== undefined) {
            // Ordinary-routing identity: the serving build must carry a real
            // identity; an "unknown" build never proves the recorded target.
            check(
              typeof sha === "string" && SHA40_PATTERN.test(sha),
              "known serving build sha",
              "missing or unknown build identity",
            );
          }
        }
      } else if (route.body) {
        check(route.body.test(body), `body matching ${route.body}`, "body did not match");
      }
      if (routeFailures === 0) log(`PASS ${route.path}`);
    } catch (error) {
      check(
        false,
        "HTTP response and body within timeout",
        error instanceof Error ? error.name : "request error",
      );
    }
  }
  log(`smoke: ${routes.length} routes, ${failures} failed assertions`);
  return failures === 0;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2);
  const [baseUrl, ...flags] = args;
  let allowIndexable = false;
  let versionOverride;
  let expectedVersionId;
  let expectedBuildSha;
  let usage = false;
  for (let i = 0; i < flags.length; i += 1) {
    if (flags[i] === "--allow-indexable") {
      allowIndexable = true;
    } else if (flags[i] === "--version-override") {
      const value = flags[i + 1];
      if (value === undefined) {
        usage = true;
      } else {
        versionOverride = value;
      }
      i += 1;
    } else if (flags[i] === "--expected-version-id") {
      const value = flags[i + 1];
      if (value === undefined) {
        usage = true;
      } else {
        expectedVersionId = value;
      }
      i += 1;
    } else if (flags[i] === "--expected-build-sha") {
      const value = flags[i + 1];
      if (value === undefined) {
        usage = true;
      } else {
        expectedBuildSha = value;
      }
      i += 1;
    } else {
      usage = true;
    }
  }
  if (typeof baseUrl !== "string" || usage) {
    console.error(
      'Usage: node bin/smoke.mjs <base-url> [--allow-indexable] [--version-override worker-name="<version-uuid>"] [--expected-version-id <lowercase-uuid>] [--expected-build-sha <40-hex>]',
    );
    process.exitCode = 2;
  } else {
    try {
      process.exitCode = (await smoke(baseUrl, {
        allowIndexable,
        versionOverride,
        expectedVersionId,
        expectedBuildSha,
      }))
        ? 0
        : 1;
    } catch (error) {
      console.error(`smoke: ${error instanceof Error ? error.message : "invalid base-url"}`);
      process.exitCode = 2;
    }
  }
}
