// Execute the real minified SDK package with local host stubs and instrumented
// Wasm exports/imports. This is NOT Cloudflare Worker or physical-close proof.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import vm from "node:vm";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const build = resolve(process.argv[2] ?? resolve(root, "build"));
const bundle = process.argv[3] && resolve(process.argv[3]);
const alias = await readFile(resolve(build, "worker/shim.mjs"), "utf8");
// Only worker-build's normal panic-recovery/module output is qualified here.
assert.match(alias, /export \* from ['"]\.\.\/index\.js['"]/);
assert.match(alias, /export \{ default \} from ['"]\.\.\/index\.js['"]/);
const packageInfo = JSON.parse(await readFile(resolve(build, "package.json"), "utf8"));
assert.equal(packageInfo.type, "module");
const events = [];
const errors = [];
let urlImports = 0;
let panicFetch = false;
let instance;
const actualWebAssembly = WebAssembly;
const wasmApi = Object.create(WebAssembly);
wasmApi.Instance = class {
  constructor(module, imports) {
    for (const namespace of Object.values(imports)) {
      for (const name of Object.keys(namespace)) {
        if (name.startsWith("__wbg_url_")) {
          const original = namespace[name];
          namespace[name] = (...args) => {
            urlImports++;
            return original(...args);
          };
        }
      }
    }
    instance = new actualWebAssembly.Instance(module, imports);
    this.exports = { ...instance.exports };
    for (const name of ["nntpaccount_new", "nntpaccount_fetch", "nntpaccount_alarm"]) {
      assert.equal(this.exports[name], undefined, `unqualified account export ${name}`);
    }
    for (const name of ["nntppool_new", "nntppool_fetch", "nntppool_alarm", "fetch"]) {
      assert.equal(typeof this.exports[name], "function", `missing Wasm export ${name}`);
      const original = this.exports[name];
      this.exports[name] = (...args) => {
        events.push({ name, args });
        if (panicFetch && name.endsWith("_fetch")) {
          panicFetch = false;
          throw new WebAssembly.RuntimeError("synthetic package recovery fixture");
        }
        return original(...args);
      };
    }
  }
};
class WorkerEntrypoint {
  constructor(ctx, env) { this.ctx = ctx; this.env = env; }
}
const context = vm.createContext({
  WebAssembly: wasmApi, Request, Response, Headers, URL, URLSearchParams,
  TextEncoder, TextDecoder, ReadableStream, WritableStream, TransformStream,
  AbortController, AbortSignal, crypto: globalThis.crypto, performance,
  setTimeout, clearTimeout, queueMicrotask,
  addEventListener() {},
  console: { log() {}, error(...args) { errors.push(args); }, warn() {} },
});
const modules = new Map();
async function load(path) {
  if (modules.has(path)) return modules.get(path);
  let mod;
  if (path === "cloudflare:workers") {
    mod = new vm.SyntheticModule(["WorkerEntrypoint"], function () {
      this.setExport("WorkerEntrypoint", WorkerEntrypoint);
    }, { context });
  } else if (path === "cloudflare:sockets") {
    mod = new vm.SyntheticModule(["connect"], function () {
      this.setExport("connect", () => { throw new Error("network forbidden in package fixture"); });
    }, { context });
  } else if (path.endsWith(".wasm")) {
    const compiled = await WebAssembly.compile(await readFile(path));
    mod = new vm.SyntheticModule(["default"], function () {
      this.setExport("default", compiled);
    }, { context });
  } else {
    mod = new vm.SourceTextModule(await readFile(path, "utf8"), { context, identifier: path });
  }
  modules.set(path, mod);
  await mod.link((specifier, parent) => {
    if (specifier.startsWith("cloudflare:")) return load(specifier);
    const path = parent.identifier === resolve(root, "entry.mjs")
      && specifier === "./build/worker/shim.mjs"
      ? resolve(build, "worker/shim.mjs") : resolve(dirname(parent.identifier), specifier);
    return load(path);
  });
  return mod;
}

const sdk = await load(resolve(build, "worker/shim.mjs"));
await sdk.evaluate();
function count(name) { return events.filter((event) => event.name === name).length; }
async function recover(ctor, object, prefix, state, env) {
  panicFetch = true;
  assert.throws(() => object.fetch(new Request("https://fixture.invalid/missing")),
    /synthetic package recovery fixture/);
  const epoch = context.__worker_init_state.instanceId;
  const creations = count(`${prefix}_new`);
  new ctor(state, env); // the existing SDK constructor performs reset
  const epochAdvance = context.__worker_init_state.instanceId - epoch;
  assert.ok(epochAdvance > 0);
  const recovered = await object.fetch(new Request("https://fixture.invalid/missing"));
  assert.equal(recovered.status, 404);
  return { creations: count(`${prefix}_new`) - creations, epochAdvance };
}
const originals = new Map();
const recovery = new Map();
assert.equal(sdk.namespace.NntpAccount, undefined);
for (const name of ["NntpPool"]) {
  const ctor = sdk.namespace[name];
  assert.equal(typeof ctor, "function");
  originals.set(name, Object.getOwnPropertyDescriptors(ctor.prototype));
  const state = { id: { name: undefined }, storage: {} };
  const env = {};
  // Compare against the actual unmodified SDK, not an assumed proxy lifecycle.
  recovery.set(name, await recover(ctor, new ctor(state, env), name.toLowerCase(), state, env));
}
const entry = await load(bundle ?? resolve(root, "entry.mjs"));
await entry.evaluate();
// A bundle includes its own SDK instance, so compare identity in the unbundled
// graph; the same behavioral bank below also runs on the final bundled entry.
if (!bundle) {
  assert.deepEqual(Object.keys(entry.namespace), Object.keys(sdk.namespace));
  for (const name of Object.keys(sdk.namespace)) assert.equal(entry.namespace[name], sdk.namespace[name]);
  for (const [name, descriptors] of originals) {
    const after = Object.getOwnPropertyDescriptors(entry.namespace[name].prototype);
    assert.deepEqual(Object.keys(after), Object.keys(descriptors));
    for (const key of Object.keys(after)) assert.deepEqual(after[key], descriptors[key]);
  }
}

let checks = 0;
assert.equal(entry.namespace.NntpAccount, undefined);
for (const [name, prefix, path] of [["NntpPool", "nntppool", "/probe"]]) {
  const state = { id: { name: undefined }, storage: {} };
  const env = {};
  const object = new entry.namespace[name](state, env);
  const creation = events.filter((event) => event.name === `${prefix}_new`).at(-1);
  assert.deepEqual(creation.args, [state, env]);
  checks++;
  // The decorator remains off-route; this package preserves existing dispatch.
  for (const route of [path, "/not-a-route", "/internal/account/body"]) {
    const request = new Request(`https://fixture.invalid${route}`);
    const before = count(`${prefix}_fetch`);
    const imports = urlImports;
    const response = await object.fetch(request);
    assert.equal(response.status, 404); // harmless methods/absent auth; no operation is admitted
    assert.equal(count(`${prefix}_fetch`) - before, 1);
    assert.equal(urlImports - imports, 1);
    checks++;
  }
  // Calling the real SDK alarm with an unowned object performs no account or
  // meter I/O. Test its unchanged dispatch, not actual platform alarm delivery.
  state.storage.list = async () => new Map();
  const before = count(`${prefix}_alarm`);
  const alarm = await object.alarm();
  assert.equal(alarm.status, 200);
  assert.equal(count(`${prefix}_alarm`) - before, 1);
  checks++;
  // Inject only at the retained Wasm dispatch boundary, not by editing glue.
  // The existing SDK proxy must still catch a trap and reconstruct after reset.
  assert.deepEqual(await recover(entry.namespace[name], object, prefix, state, env), recovery.get(name));
}
const publicObject = new entry.namespace.default({}, {});
const publicBefore = count("fetch");
const publicReply = await publicObject.fetch(new Request("https://fixture.invalid/not-a-route"));
assert.equal(publicReply.status, 404);
assert.equal(count("fetch") - publicBefore, 1);
checks++;
assert.equal(errors.length, 2, "only the baseline and entry SDK traps are expected");
for (const error of errors) assert.match(String(error), /synthetic package recovery fixture/);
assert.ok(instance);
console.log(`generated package ingress: ${checks} checks passed (${bundle ? "bundled" : "module"}); local host stubs, not Worker qualification`);
