import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { test } from "node:test";
import { collectRuntimeCases } from "./collector.mjs";

const pins = JSON.parse(await readFile(resolve(".github/worker-toolchain.json"), "utf8")).runtime;
const sha = (text) => createHash("sha256").update(text).digest("hex");

async function fixture(t, behavior = {}) {
  const dir = await mkdtemp(join(tmpdir(), "collector-test-"));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const bundle = join(dir, "bundle");
  await mkdir(join(bundle, "worker"), { recursive: true });
  await writeFile(join(bundle, "worker/shim.mjs"), "original shim");
  await writeFile(join(bundle, "worker/main.wasm"), "original wasm");
  const entry = join(dir, "entry.mjs");
  await writeFile(entry, "probe entry");
  const calls = [];
  class Runtime {
    constructor(options) {
      calls.push(["start", options]);
      if (behavior.stdio) {
        const stream = { on(event, callback) { callback(Buffer.alloc(100000, "x")); } };
        options.handleRuntimeStdio(stream, stream);
      }
      if (behavior.startupThrow) throw new Error("startup failed");
      this.ready = behavior.ready || Promise.resolve();
    }
    async dispatchFetch(url, options) {
      const name = options.headers["x-probe-case"];
      calls.push(["case", name]);
      if (behavior.dispatchThrow) throw new Error("dispatch failed");
      if (behavior.dispatchHang) return new Promise(() => {});
      const text = behavior.oversized ? "x".repeat(1024 * 1024 + 1) : behavior.malformed ? "not json" : JSON.stringify({ name });
      return new Response(text, { status: 200 });
    }
    async dispose() {
      calls.push(["dispose"]);
      if (behavior.disposeThrow) throw new Error("dispose failed");
      if (behavior.disposeHang) return new Promise(() => {});
    }
  }
  const loadRuntime = async () => ({ Miniflare: Runtime,
    versions: { miniflare: pins.miniflare, workerd: behavior.version || pins.workerd },
    runtimeBinary: { path: "/synthetic/workerd", sha256: "a".repeat(64) } });
  const args = { bundle, entry, sourceSha: "b".repeat(40), nodeRoot: dir, out: join(dir, "out"),
    compatibilityDate: "2026-01-01", cases: ["body-error", "get", "transition"] };
  return { args, calls, loadRuntime };
}

test("copies unchanged bundle, records provenance and dispatches sequentially", async (t) => {
  const f = await fixture(t);
  const evidence = await collectRuntimeCases(f.args, { loadRuntime: f.loadRuntime });
  assert.deepEqual(evidence.errors, []);
  assert.deepEqual(f.calls.filter(([kind]) => kind === "case").map(([, name]) => name), f.args.cases);
  assert.equal(evidence.generatedHashes["worker/shim.mjs"], sha("original shim"));
  assert.equal(evidence.generatedHashes["worker/main.wasm"], sha("original wasm"));
  assert.equal(evidence.probeEntrySha256, sha("probe entry"));
  assert.equal(evidence.runtimeBinary.sha256, "a".repeat(64));
  assert.deepEqual(evidence.cleanup, { disposed: true, removed: true });
  assert.deepEqual(JSON.parse(await readFile(join(f.args.out, "evidence.json"), "utf8")), evidence);
  assert.equal(await readFile(join(f.args.bundle, "worker/shim.mjs"), "utf8"), "original shim");
});

test("default resolver records the selected package binary", async (t) => {
  const f = await fixture(t);
  const modules = join(f.args.nodeRoot, "node_modules");
  for (const name of ["workerd", "miniflare"]) {
    await mkdir(join(modules, name), { recursive: true });
    await writeFile(join(modules, name, "package.json"), JSON.stringify({ name, version: pins[name], main: "index.cjs" }));
  }
  await writeFile(join(modules, "workerd/binary"), "synthetic binary");
  await writeFile(join(modules, "workerd/index.cjs"), 'exports.default = require("node:path").join(__dirname, "binary");');
  await writeFile(join(modules, "miniflare/index.cjs"), 'exports.Miniflare = class { get ready() { return Promise.resolve(); } async dispatchFetch() { return new Response("{}") } async dispose() {} };');
  const result = await collectRuntimeCases(f.args);
  assert.deepEqual(result.errors, []);
  assert.equal(result.runtimeBinary.sha256, sha("synthetic binary"));
});

test("runtime stdio is bounded", async (t) => {
  const f = await fixture(t, { stdio: true });
  const result = await collectRuntimeCases(f.args, { loadRuntime: f.loadRuntime });
  assert.equal(Buffer.byteLength(result.runtimeStdio), 65536);
});

test("pin mismatch fails before startup", async (t) => {
  const f = await fixture(t, { version: "wrong" });
  const result = await collectRuntimeCases(f.args, { loadRuntime: f.loadRuntime });
  assert.match(result.errors.join(), /pin mismatch/);
  assert.deepEqual(f.calls, []);
});

for (const [name, behavior, pattern] of [
  ["startup failure", { startupThrow: true }, /startup failed/],
  ["dispatch failure", { dispatchThrow: true }, /dispatch failed/],
  ["malformed output", { malformed: true }, /JSON/],
  ["oversized output", { oversized: true }, /exceeds 1 MiB/],
  ["disposal failure", { disposeThrow: true }, /dispose failed/],
]) {
  test(name, async (t) => {
    const f = await fixture(t, behavior);
    const result = await collectRuntimeCases(f.args, { loadRuntime: f.loadRuntime });
    assert.match(result.errors.join(), pattern);
    assert.equal(result.cleanup.removed, true);
    if (!behavior.startupThrow) assert.ok(f.calls.some(([kind]) => kind === "dispose"));
    if (behavior.dispatchThrow || behavior.malformed) assert.equal(f.calls.filter(([kind]) => kind === "case").length, 1);
  });
}

test("startup timeout still disposes and removes the copy", async (t) => {
  const f = await fixture(t, { ready: new Promise(() => {}) });
  const result = await collectRuntimeCases(f.args, { loadRuntime: f.loadRuntime });
  assert.match(result.errors.join(), /startup timeout/);
  assert.deepEqual(result.cleanup, { disposed: true, removed: true });
});

test("dispatch timeout stops subsequent cases and disposes", async (t) => {
  const f = await fixture(t, { dispatchHang: true });
  const result = await collectRuntimeCases(f.args, { loadRuntime: f.loadRuntime });
  assert.match(result.errors.join(), /case body-error timeout/);
  assert.equal(f.calls.filter(([kind]) => kind === "case").length, 1);
  assert.deepEqual(result.cleanup, { disposed: true, removed: true });
});

test("disposal timeout is an error and the copy is removed", async (t) => {
  const f = await fixture(t, { disposeHang: true });
  const result = await collectRuntimeCases(f.args, { loadRuntime: f.loadRuntime });
  assert.match(result.errors.join(), /disposal timeout/);
  assert.deepEqual(result.cleanup, { disposed: false, removed: true });
});

test("evidence write failure is a collection error", async (t) => {
  const f = await fixture(t);
  f.args.out = f.args.entry;
  const result = await collectRuntimeCases(f.args, { loadRuntime: f.loadRuntime });
  assert.match(result.errors.join(), /evidence write/);
  assert.equal(result.cleanup.disposed, true);
});
