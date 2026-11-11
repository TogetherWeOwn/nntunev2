// Facade composition contract only. This is NOT the generated build artifact,
// workerd binding dispatch or a production isolation/qualification receipt.
import assert from "node:assert/strict";
import fs from "node:fs";
import vm from "node:vm";
import test from "node:test";

async function facade({ bridge = true, fail = false } = {}) {
  let publicCalls = 0;
  let privateCalls = 0;
  const constructions = [];
  class PlatformEntry {
    constructor(ctx, env) { this.ctx = ctx; this.env = env; }
  }
  class Generated {
    constructor(ctx, env) { constructions.push([ctx, env]); }
    fetch(request) { publicCalls++; return new Response(request.method, { status: 404 }); }
    unrelatedRawExport() { throw new Error("must not be exposed"); }
  }
  if (bridge) {
    Generated.prototype.ingest_preparation_plan = function (request, env) {
      privateCalls++;
      if (fail) throw new Error("synthetic lifecycle fault");
      assert.equal(env.GENERATION_PREPARATION_MODE, "preparation_only");
      return new Response(request.method, { status: 200 });
    };
  }
  const context = vm.createContext({ Response });
  const platform = new vm.SyntheticModule(["WorkerEntrypoint"], function () {
    this.setExport("WorkerEntrypoint", PlatformEntry);
  }, { context });
  const generated = new vm.SyntheticModule(["default"], function () {
    this.setExport("default", Generated);
  }, { context });
  const source = fs.readFileSync(new URL("../src/runtime.mjs", import.meta.url), "utf8");
  const outer = new vm.SourceTextModule(source, { context });
  await outer.link((specifier) => {
    if (specifier === "cloudflare:workers") return platform;
    if (specifier === "../build/worker/shim.mjs") return generated;
    throw new Error("unexpected dependency");
  });
  await outer.evaluate();
  return { exports: outer.namespace, counts: () => [publicCalls, privateCalls], constructions };
}

test("only facade exports; generated RPC methods never become inherited capability", async () => {
  const { exports } = await facade();
  assert.deepEqual(Object.keys(exports).sort(), ["IngestPreparationPlans", "default"]);
  for (const Entry of [exports.default, exports.IngestPreparationPlans]) {
    assert.deepEqual(Object.getOwnPropertyNames(Entry.prototype).sort(), ["constructor", "fetch"]);
    const instance = new Entry({}, {});
    assert.equal(instance.ingest_preparation_plan, undefined);
    assert.equal(instance.unrelatedRawExport, undefined);
  }
});

test("public paths methods bodies and headers cannot select private bridge", async () => {
  const f = await facade();
  const ctx = {}; const env = { GENERATION_PREPARATION_MODE: "preparation_only" };
  const publicEntry = new f.exports.default(ctx, env);
  for (const path of ["/", "/freeze", "/open", "/ingest_preparation_plan"]) {
    for (const method of ["GET", "POST", "PUT"]) {
      const request = new Request(`https://public.invalid${path}`, {
        method, headers: { "x-entrypoint": "IngestPreparationPlans" },
        ...(method === "GET" ? {} : { body: "freeze" }),
      });
      assert.equal((await publicEntry.fetch(request)).status, 404);
    }
  }
  assert.deepEqual(f.counts(), [12, 0]);
  assert.ok(f.constructions.every(([c, e]) => c === ctx && e === env));
});

test("private absent unknown disabled and unsupported bridge refuse", async () => {
  const f = await facade();
  for (const value of [undefined, "disabled", "unknown", ""]) {
    const env = { GENERATION_PREPARATION_MODE: value };
    const privateEntry = new f.exports.IngestPreparationPlans({}, env);
    assert.equal((await privateEntry.fetch(new Request("https://private.invalid/"))).status, 503);
  }
  assert.deepEqual(f.counts(), [0, 0]);
  const unsupported = await facade({ bridge: false });
  const privateEntry = new unsupported.exports.IngestPreparationPlans({}, { GENERATION_PREPARATION_MODE: "preparation_only" });
  assert.equal((await privateEntry.fetch(new Request("https://private.invalid/"))).status, 503);
});

test("named delegation supplies server context env and contains bridge faults", async () => {
  const f = await facade();
  const ctx = {}; const env = { GENERATION_PREPARATION_MODE: "preparation_only" };
  const privateEntry = new f.exports.IngestPreparationPlans(ctx, env);
  assert.equal((await privateEntry.fetch(new Request("https://private.invalid/", { method: "POST" }))).status, 200);
  assert.deepEqual(f.counts(), [0, 1]);
  assert.equal(f.constructions[0][0], ctx); assert.equal(f.constructions[0][1], env);
  const failed = await facade({ fail: true });
  assert.equal((await new failed.exports.IngestPreparationPlans(ctx, env).fetch(new Request("https://private.invalid/"))).status, 503);
});
