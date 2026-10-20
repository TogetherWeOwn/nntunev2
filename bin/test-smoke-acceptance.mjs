// Acceptance tests for the production smoke checker (bin/smoke.mjs):
// bounded response reads and secret-safe diagnostics.
//
// Run with `node --test bin/test-smoke-acceptance.mjs` (node builtins
// only, no install, no network beyond a loopback fixture server).
// Never point these tests at staging or production.
//
// Acceptance contract pinned here (the implementation must meet it):
// - JSON probes accept at most 16 KiB of body bytes; HTML probes at
//   most 1 MiB. The ceiling applies to streamed bytes actually read,
//   including chunked replies with no Content-Length; a body of
//   exactly the ceiling still passes, one byte over fails the route.
// - On overflow the checker stops consuming and cancels/releases the
//   response reader instead of reading the body to the end.
// - One deadline (timeoutMs) covers headers plus body consumption;
//   it is not refreshed per chunk.
// - Diagnostics are fixed route/category/status text only. Arbitrary
//   response keys, response values, exception messages and raw CLI
//   arguments never reach the log sink, stdout or stderr.
// - Existing behavior is retained: exact key shapes, ok/build-SHA
//   checks, manual redirects (a redirect is a failure), single
//   attempt per route, version-override header on every probe,
//   expected-SHA equality, exit 0/1/2 (pass/probe-fail/usage).
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { fileURLToPath } from "node:url";
import { smoke } from "./smoke.mjs";

const smokeCli = fileURLToPath(new URL("./smoke.mjs", import.meta.url));

const JSON_CAP = 16 * 1024;
const HTML_CAP = 1024 * 1024;
const GOOD_SHA = "a".repeat(40);

// Synthetic sentinels only. A single substring check for "SENTINEL"
// catches every leak shape below (token, email, URL/query, embedded
// newline/control characters, raw CLI arguments).
const SENTINEL_TOKEN = "SENTINEL-TOKEN-7f3a";
const SENTINEL_EMAIL = "SENTINEL-user@example.invalid";
const SENTINEL_URL = "https://SENTINEL-host.example.invalid/x?key=SENTINEL-QUERY";
const SENTINEL_NEWLINE = "SENTINEL-LINE1\nSENTINEL-LINE2";
const SENTINEL_CTRL = "SENTINEL-CTRL\u0007END";

const HTML_MARKERS = "NNTune Get started sign up plan pricing Sign in byop server host";

function happyBody(path) {
  if (path === "/api/health") return { type: "json", body: { ok: true, version: "1" } };
  if (path === "/api/ready") return { type: "json", body: { ok: true } };
  if (path === "/api/build") return { type: "json", body: { sha: GOOD_SHA } };
  return { type: "text", body: `<html><body>${HTML_MARKERS}</body></html>` };
}

// Start a loopback fixture server. `routes` maps a path to a handler
// (req, res) => void overriding the happy default for that path.
// Records every hit (path + headers) for single-attempt and
// version-override assertions.
async function withFixture(routes, fn) {
  const hits = [];
  const server = createServer((req, res) => {
    const path = new URL(req.url, "http://127.0.0.1").pathname;
    hits.push({ path, headers: req.headers });
    const override = routes[path];
    if (override) {
      override(req, res);
      return;
    }
    const happy = happyBody(path);
    if (happy.type === "json") {
      const text = JSON.stringify(happy.body);
      res.writeHead(200, { "content-type": "application/json", "content-length": Buffer.byteLength(text) });
      res.end(text);
    } else {
      res.writeHead(200, { "content-type": "text/html", "content-length": Buffer.byteLength(happy.body) });
      res.end(happy.body);
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const base = `http://127.0.0.1:${server.address().port}`;
  try {
    return await fn({ base, hits });
  } finally {
    server.closeAllConnections?.();
    await new Promise((resolve) => server.close(resolve));
  }
}

function captureLog() {
  const lines = [];
  return { lines, log: (line) => lines.push(String(line)) };
}

function assertNoSentinel(text, label) {
  assert.ok(!text.includes("SENTINEL"), `${label} leaked a sentinel: ${JSON.stringify(text.slice(0, 200))}`);
}

function assertNoSentinelInLines(lines, label) {
  for (const line of lines) assertNoSentinel(line, label);
  // One log call must be one printable line: no embedded newline or
  // control characters smuggled in from an untrusted value.
  for (const line of lines) {
    assert.ok(!/[\u0000-\u001f\u007f]/.test(line), `${label} log line carries control characters`);
  }
}

function runCli(args) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [smokeCli, ...args], { encoding: "utf8" });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => (stdout += chunk));
    child.stderr.on("data", (chunk) => (stderr += chunk));
    child.on("error", reject);
    child.on("close", (code) => resolve({ code, stdout, stderr }));
  });
}

describe("bounded response reads", () => {
  it("accepts a JSON body of exactly the byte ceiling", async () => {
    const core = JSON.stringify({ ok: true, version: "1" });
    const body = core + " ".repeat(JSON_CAP - core.length);
    assert.equal(Buffer.byteLength(body), JSON_CAP);
    await withFixture(
      {
        "/api/health": (req, res) => {
          res.writeHead(200, { "content-type": "application/json", "content-length": Buffer.byteLength(body) });
          res.end(body);
        },
      },
      async ({ base }) => {
        const { lines, log } = captureLog();
        const failures = await smoke(base, { log, timeoutMs: 2_000 });
        assert.equal(failures, 0, lines.join("\n"));
      },
    );
  });

  it("fails a JSON body one byte over the ceiling (declared length)", async () => {
    const core = JSON.stringify({ ok: true, version: "1" });
    const body = core + " ".repeat(JSON_CAP + 1 - core.length);
    await withFixture(
      {
        "/api/health": (req, res) => {
          res.writeHead(200, { "content-type": "application/json", "content-length": Buffer.byteLength(body) });
          res.end(body);
        },
      },
      async ({ base }) => {
        const { lines, log } = captureLog();
        const failures = await smoke(base, { log, timeoutMs: 2_000 });
        assert.ok(failures >= 1, "oversized JSON must not be promotion-ready");
        assert.ok(!lines.some((l) => l.startsWith("smoke ok")), lines.join("\n"));
      },
    );
  });

  it("fails a JSON body over the ceiling sent chunked with no Content-Length", async () => {
    const core = JSON.stringify({ ok: true });
    await withFixture(
      {
        "/api/ready": (req, res) => {
          res.writeHead(200, { "content-type": "application/json" });
          res.write(core);
          // Trailing whitespace is valid JSON padding; only the byte
          // ceiling can reject this body.
          for (let sent = core.length; sent < JSON_CAP + 4096; sent += 4096) res.write(" ".repeat(4096));
          res.end();
        },
      },
      async ({ base }) => {
        const { log } = captureLog();
        const failures = await smoke(base, { log, timeoutMs: 2_000 });
        assert.ok(failures >= 1, "chunked oversized JSON must fail");
      },
    );
  });

  it("accepts an HTML body of exactly the byte ceiling", async () => {
    const body = HTML_MARKERS + "a".repeat(HTML_CAP - HTML_MARKERS.length);
    assert.equal(Buffer.byteLength(body), HTML_CAP);
    await withFixture(
      {
        "/": (req, res) => {
          res.writeHead(200, { "content-type": "text/html", "content-length": Buffer.byteLength(body) });
          res.end(body);
        },
      },
      async ({ base }) => {
        const { lines, log } = captureLog();
        const failures = await smoke(base, { log, timeoutMs: 2_000 });
        assert.equal(failures, 0, lines.join("\n"));
      },
    );
  });

  it("fails an HTML body one byte over the ceiling even though the marker matches", async () => {
    const body = HTML_MARKERS + "a".repeat(HTML_CAP + 1 - HTML_MARKERS.length);
    await withFixture(
      {
        "/": (req, res) => {
          res.writeHead(200, { "content-type": "text/html", "content-length": Buffer.byteLength(body) });
          res.end(body);
        },
      },
      async ({ base }) => {
        const { log } = captureLog();
        const failures = await smoke(base, { log, timeoutMs: 2_000 });
        assert.ok(failures >= 1, "oversized HTML must not be promotion-ready");
      },
    );
  });

  it("fails an HTML body over the ceiling sent chunked, marker first", async () => {
    await withFixture(
      {
        "/pricing": (req, res) => {
          res.writeHead(200, { "content-type": "text/html" });
          res.write(HTML_MARKERS);
          for (let sent = HTML_MARKERS.length; sent < HTML_CAP + 65536; sent += 65536) {
            res.write("a".repeat(65536));
          }
          res.end();
        },
      },
      async ({ base }) => {
        const { log } = captureLog();
        const failures = await smoke(base, { log, timeoutMs: 2_000 });
        assert.ok(failures >= 1, "chunked oversized HTML must fail");
      },
    );
  });

  it("cancels the reader on overflow instead of consuming the whole stream", async () => {
    const CHUNK = 64 * 1024;
    const MAX_PULLS = 512; // 32 MiB offered if fully consumed
    let pulls = 0;
    let cancelled = false;
    const stream = new ReadableStream({
      pull(controller) {
        if (pulls >= MAX_PULLS) {
          controller.close();
          return;
        }
        pulls += 1;
        const chunk = new Uint8Array(CHUNK).fill(97);
        if (pulls === 1) chunk.set(new TextEncoder().encode(HTML_MARKERS), 0);
        controller.enqueue(chunk);
      },
      cancel() {
        cancelled = true;
      },
    });
    const realFetch = globalThis.fetch;
    globalThis.fetch = async (url) => {
      const path = new URL(String(url)).pathname;
      if (path === "/") return new Response(stream, { status: 200, headers: { "content-type": "text/html" } });
      const happy = happyBody(path);
      return happy.type === "json"
        ? Response.json(happy.body)
        : new Response(happy.body, { status: 200, headers: { "content-type": "text/html" } });
    };
    try {
      const { log } = captureLog();
      const failures = await smoke("http://127.0.0.1:9", { log, timeoutMs: 5_000 });
      assert.ok(failures >= 1, "overflowing stream must fail the route");
      assert.equal(cancelled, true, "reader must be cancelled/released on overflow");
      assert.ok(
        pulls * CHUNK < 8 * 1024 * 1024,
        `checker retained/read ${pulls * CHUNK} bytes before rejecting a 1 MiB-capped route`,
      );
    } finally {
      globalThis.fetch = realFetch;
    }
  });

  it("keeps one deadline across a trickling body (no fresh allowance per chunk)", { timeout: 15_000 }, async () => {
    await withFixture(
      {
        "/login": (req, res) => {
          res.writeHead(200, { "content-type": "text/html" });
          res.write(HTML_MARKERS);
          const timer = setInterval(() => res.write("a".repeat(1024)), 80);
          req.on("close", () => clearInterval(timer));
          res.on("close", () => clearInterval(timer));
        },
      },
      async ({ base }) => {
        const { log } = captureLog();
        const started = Date.now();
        const failures = await smoke(base, { log, timeoutMs: 300 });
        const elapsed = Date.now() - started;
        assert.ok(failures >= 1, "a body that never finishes within the deadline must fail");
        assert.ok(elapsed < 3_000, `one 300ms-deadline route took ${elapsed}ms (per-chunk reset?)`);
      },
    );
  });

  it("fails a route whose headers stall past the deadline", { timeout: 15_000 }, async () => {
    await withFixture(
      {
        "/api/health": (req, res) => {
          setTimeout(() => {
            if (!res.destroyed) {
              res.writeHead(200, { "content-type": "application/json" });
              res.end(JSON.stringify({ ok: true, version: "1" }));
            }
          }, 800);
        },
      },
      async ({ base }) => {
        const { log } = captureLog();
        const started = Date.now();
        const failures = await smoke(base, { log, timeoutMs: 250 });
        assert.ok(failures >= 1, "stalled headers must fail");
        assert.ok(Date.now() - started < 3_000, "stalled route must give up at the deadline");
      },
    );
  });
});

describe("secret-safe diagnostics", () => {
  it("never logs untrusted response keys", async () => {
    const hostile = { ok: true, [SENTINEL_TOKEN]: 1, [SENTINEL_NEWLINE]: 2, [SENTINEL_CTRL]: 3 };
    await withFixture(
      {
        "/api/ready": (req, res) => {
          const text = JSON.stringify(hostile);
          res.writeHead(200, { "content-type": "application/json", "content-length": Buffer.byteLength(text) });
          res.end(text);
        },
      },
      async ({ base }) => {
        const { lines, log } = captureLog();
        const failures = await smoke(base, { log, timeoutMs: 2_000 });
        assert.ok(failures >= 1, "extra keys must fail the exact-shape check");
        assertNoSentinelInLines(lines, "response-key");
        assert.ok(
          lines.some((l) => l.includes("/api/ready")),
          "diagnostic must still name the fixed route",
        );
      },
    );
  });

  it("never logs untrusted response values (health/ready/build fields)", async () => {
    const json = (obj) => (req, res) => {
      const text = JSON.stringify(obj);
      res.writeHead(200, { "content-type": "application/json", "content-length": Buffer.byteLength(text) });
      res.end(text);
    };
    await withFixture(
      {
        "/api/health": json({ ok: SENTINEL_TOKEN, version: SENTINEL_EMAIL }),
        "/api/ready": json({ ok: SENTINEL_URL }),
        "/api/build": json({ sha: `SENTINEL-SHA?next=${SENTINEL_NEWLINE}` }),
      },
      async ({ base }) => {
        const { lines, log } = captureLog();
        const failures = await smoke(base, { log, timeoutMs: 2_000, expectedSha: GOOD_SHA });
        assert.ok(failures >= 3, "hostile field values must fail their checks");
        assertNoSentinelInLines(lines, "response-value");
        for (const path of ["/api/health", "/api/ready", "/api/build"]) {
          assert.ok(lines.some((l) => l.includes(path)), `diagnostic must name ${path}`);
        }
      },
    );
  });

  it("never logs raw exception messages from a failing fetch", async () => {
    const realFetch = globalThis.fetch;
    globalThis.fetch = async () => {
      throw new Error(`boom ${SENTINEL_TOKEN} ${SENTINEL_EMAIL} ${SENTINEL_URL}\n${SENTINEL_CTRL}`);
    };
    try {
      const { lines, log } = captureLog();
      const failures = await smoke("http://127.0.0.1:9", { log, timeoutMs: 2_000 });
      assert.ok(failures >= 8, "every route must fail when fetch throws");
      assertNoSentinelInLines(lines, "exception-message");
      for (const path of ["/api/health", "/api/ready", "/api/build", "/", "/login"]) {
        assert.ok(lines.some((l) => l.includes(path)), `diagnostic must name ${path}`);
      }
    } finally {
      globalThis.fetch = realFetch;
    }
  });

  it("CLI usage errors never echo raw arguments", async () => {
    const cases = [
      [`--${SENTINEL_TOKEN}`],
      ["http://127.0.0.1:9", `extra\n${SENTINEL_NEWLINE}`],
      ["http://127.0.0.1:9", "--expected-sha", SENTINEL_TOKEN],
      ["http://127.0.0.1:9", "--version-override", `${SENTINEL_EMAIL}="x"`],
      [`https://user:${SENTINEL_TOKEN}@127.0.0.1:9/`],
      [`http://127.0.0.1:9/path?key=SENTINEL-QUERY`],
    ];
    for (const args of cases) {
      const { code, stdout, stderr } = await runCli(args);
      assert.equal(code, 2, `args ${JSON.stringify(args.map((a) => a.slice(0, 24)))} must be a usage error`);
      assertNoSentinel(stdout + stderr, "cli-usage");
    }
  });
});

describe("retained contract", () => {
  it("passes a fully happy fixture, one attempt per route", async () => {
    await withFixture({}, async ({ base, hits }) => {
      const { lines, log } = captureLog();
      const failures = await smoke(base, { log, timeoutMs: 2_000, expectedSha: GOOD_SHA });
      assert.equal(failures, 0, lines.join("\n"));
      assert.equal(hits.length, 8, "single attempt: exactly one hit per route");
      assert.equal(new Set(hits.map((h) => h.path)).size, 8);
    });
  });

  it("sends the version-override header on every probe", async () => {
    const override = 'worker="12345678-1234-1234-1234-123456789abc"';
    await withFixture({}, async ({ base, hits }) => {
      const { log } = captureLog();
      const failures = await smoke(base, { log, timeoutMs: 2_000, versionOverride: override });
      assert.equal(failures, 0);
      assert.equal(hits.length, 8);
      for (const hit of hits) {
        assert.equal(
          hit.headers["cloudflare-workers-version-overrides"],
          override,
          `missing version override on ${hit.path}`,
        );
      }
    });
  });

  it("treats a redirect as a failure (manual redirect handling)", async () => {
    await withFixture(
      {
        "/api/health": (req, res) => {
          res.writeHead(302, { location: "/api/ready" });
          res.end();
        },
      },
      async ({ base }) => {
        const { log } = captureLog();
        const failures = await smoke(base, { log, timeoutMs: 2_000 });
        assert.ok(failures >= 1, "redirect replies cannot be promotion-ready");
      },
    );
  });

  it("fails malformed JSON", async () => {
    await withFixture(
      {
        "/api/ready": (req, res) => {
          res.writeHead(200, { "content-type": "application/json" });
          res.end("{not json");
        },
      },
      async ({ base }) => {
        const { log } = captureLog();
        assert.ok((await smoke(base, { log, timeoutMs: 2_000 })) >= 1);
      },
    );
  });

  it("fails wrong exact shapes: missing key, ok not true, array body", async () => {
    const json = (text) => (req, res) => {
      res.writeHead(200, { "content-type": "application/json", "content-length": Buffer.byteLength(text) });
      res.end(text);
    };
    for (const body of [JSON.stringify({ version: "1" }), JSON.stringify({ ok: false, version: "1" }), "[]"]) {
      await withFixture({ "/api/health": json(body) }, async ({ base }) => {
        const { log } = captureLog();
        assert.ok((await smoke(base, { log, timeoutMs: 2_000 })) >= 1, `body ${body} must fail`);
      });
    }
  });

  it("enforces expected-SHA equality; 'unknown' never satisfies it", async () => {
    const build = (sha) => (req, res) => {
      const text = JSON.stringify({ sha });
      res.writeHead(200, { "content-type": "application/json", "content-length": Buffer.byteLength(text) });
      res.end(text);
    };
    await withFixture({ "/api/build": build("unknown") }, async ({ base }) => {
      const { log } = captureLog();
      assert.ok((await smoke(base, { log, timeoutMs: 2_000, expectedSha: GOOD_SHA })) >= 1);
    });
    await withFixture({ "/api/build": build("b".repeat(40)) }, async ({ base }) => {
      const { log } = captureLog();
      assert.ok((await smoke(base, { log, timeoutMs: 2_000, expectedSha: GOOD_SHA })) >= 1);
    });
  });

  it("rejects invalid version-override / expected-sha inputs before probing", async () => {
    await withFixture({}, async ({ base, hits }) => {
      const { log } = captureLog();
      await assert.rejects(() => smoke(base, { log, versionOverride: "not-an-override" }));
      await assert.rejects(() => smoke(base, { log, expectedSha: "ABC" }));
      await assert.rejects(() => smoke("https://user:pass@127.0.0.1:9/", { log }));
      assert.equal(hits.length, 0, "no probe may fire on invalid input");
    });
  });

  it("CLI exit codes: 0 happy, 1 probe failure, 2 usage, 0 help", async () => {
    await withFixture({}, async ({ base }) => {
      const happy = await runCli([base, "--expected-sha", GOOD_SHA]);
      assert.equal(happy.code, 0, happy.stdout + happy.stderr);
      const wrongSha = await runCli([base, "--expected-sha", "b".repeat(40)]);
      assert.equal(wrongSha.code, 1);
      assertNoSentinel(wrongSha.stdout + wrongSha.stderr, "cli-probe");
    });
    assert.equal((await runCli([])).code, 2);
    assert.equal((await runCli(["--help"])).code, 0);
  });
});

describe("HTTP response guards", () => {
  // Otherwise valid bodies isolate the status guard from parsing,
  // exact-shape and HTML-marker checks. Every other route stays happy.
  for (const { kind, path, type, body } of [
    { kind: "JSON", path: "/api/health", type: "application/json", body: '{"ok":true,"version":"1"}' },
    { kind: "HTML", path: "/login", type: "text/html", body: "<html><body>Sign in</body></html>" },
  ]) {
    it(`rejects HTTP 500 ${kind} despite an otherwise valid body`, async () => {
      await withFixture(
        {
          [path]: (req, res) => {
            res.writeHead(500, { "content-type": type, "content-length": Buffer.byteLength(body) });
            res.end(body);
          },
        },
        async ({ base, hits }) => {
          const { lines, log } = captureLog();
          const failures = await smoke(base, { log, timeoutMs: 2_000 });
          assert.equal(failures, 1, `only ${path}'s HTTP 500 must fail: ${lines.join("\n")}`);
          assert.ok(!lines.some((l) => l.startsWith("smoke ok")), lines.join("\n"));
          assert.equal(hits.length, 8, "every route must still be attempted once");
          assert.equal(new Set(hits.map((h) => h.path)).size, 8);
        },
      );
    });
  }

  // These aliases would pass their original routes' content checks if
  // followed. A redirect to another probe route can fail for the wrong
  // shape even when manual redirect handling has regressed.
  for (const { kind, path, alias, type, body } of [
    {
      kind: "JSON",
      path: "/api/health",
      alias: "/health-alias",
      type: "application/json",
      body: '{"ok":true,"version":"1"}',
    },
    {
      kind: "HTML",
      path: "/login",
      alias: "/login-alias",
      type: "text/html",
      body: "<html><body>Sign in</body></html>",
    },
  ]) {
    it(`rejects ${kind} redirects without fetching a valid-content alias`, async () => {
      await withFixture(
        {
          [path]: (req, res) => {
            res.writeHead(302, { location: alias });
            res.end();
          },
          [alias]: (req, res) => {
            res.writeHead(200, { "content-type": type, "content-length": Buffer.byteLength(body) });
            res.end(body);
          },
        },
        async ({ base, hits }) => {
          const { lines, log } = captureLog();
          const failures = await smoke(base, { log, timeoutMs: 2_000 });
          assert.equal(hits.filter((h) => h.path === path).length, 1, `${path} must be attempted once`);
          assert.equal(hits.filter((h) => h.path === alias).length, 0, `${alias} must never be fetched`);
          assert.equal(failures, 1, `only ${path}'s redirect must fail: ${lines.join("\n")}`);
          assert.ok(!lines.some((l) => l.startsWith("smoke ok")), lines.join("\n"));
          assert.equal(hits.length, 8, "every original route must still be attempted once");
          assert.equal(new Set(hits.map((h) => h.path)).size, 8);
        },
      );
    });
  }

  it("rejects a small HTTP 200 login page without the Sign in marker", async () => {
    const body = "<html><body>Welcome</body></html>";
    await withFixture(
      {
        "/login": (req, res) => {
          res.writeHead(200, { "content-type": "text/html", "content-length": Buffer.byteLength(body) });
          res.end(body);
        },
      },
      async ({ base, hits }) => {
        const { lines, log } = captureLog();
        const failures = await smoke(base, { log, timeoutMs: 2_000 });
        assert.equal(failures, 1, `only /login's missing marker must fail: ${lines.join("\n")}`);
        assert.ok(!lines.some((l) => l.startsWith("smoke ok")), lines.join("\n"));
        assert.equal(hits.length, 8, "every route must still be attempted once");
        assert.equal(new Set(hits.map((h) => h.path)).size, 8);
      },
    );
  });
});
