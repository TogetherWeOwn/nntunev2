// Unit tests for the Durable Object ingress guard. The fake classes below are
// unit-only stand-ins: they are not Worker, generated SDK or runtime evidence.
// Only synthetic .invalid names are used and nothing touches the network.
import assert from "node:assert/strict";
import test from "node:test";

import { guardDurableObjects } from "../ingress.mjs";

const NAMES = ["NntpAccount", "NntpPool"];

function makeClass() {
  const calls = [];
  class Fake {
    constructor(state, env) {
      this.state = state;
      this.env = env;
    }
    fetch(request) {
      calls.push({ self: this, args: [...arguments] });
      return new Response("ok");
    }
    alarm() {
      return "alarm";
    }
    other(x) {
      return x;
    }
  }
  return { Fake, calls };
}

function makeExports() {
  const a = makeClass();
  const p = makeClass();
  return {
    exports: { NntpAccount: a.Fake, NntpPool: p.Fake },
    calls: { NntpAccount: a.calls, NntpPool: p.calls },
  };
}

function shape(prototype) {
  const out = {};
  for (const key of Reflect.ownKeys(prototype)) {
    out[String(key)] = Object.getOwnPropertyDescriptor(prototype, key);
  }
  return out;
}

// Plain object standing in for a Request whose url is exactly `url`.
const fake = (url) => ({ url });

// Pad `prefix + filler... + suffix` to exactly `n` UTF-16 units.
function fit(prefix, suffix, n, fill = "a") {
  const pad = n - prefix.length - suffix.length;
  assert.ok(pad >= 0);
  return prefix + fill.repeat(Math.floor(pad / fill.length)) + suffix;
}

function hostUrl(n) {
  const fixed = "https://".length + ".invalid/".length;
  const pad = n - fixed;
  let host = "";
  for (let i = 0; i < pad; i++) host += (i + 1) % 40 === 0 ? "." : "a";
  host = host.slice(0, -1) + "a";
  return `https://${host}.invalid/`;
}

async function admitted(proto, request) {
  const { exports, calls } = makeExports();
  guardDurableObjects(exports);
  const name = proto;
  const before = calls[name].length;
  const res = await new exports[name]({}, {}).fetch(request);
  const passed = calls[name].length === before + 1;
  if (!passed) {
    assert.equal(res.status, 404);
    assert.equal(await res.text(), "not found");
  }
  return passed;
}

async function admittedBoth(request) {
  const a = await admitted("NntpAccount", request);
  const b = await admitted("NntpPool", request);
  assert.equal(a, b);
  return a;
}

test("both distinct class prototypes are guarded independently", async () => {
  const { exports, calls } = makeExports();
  assert.notEqual(exports.NntpAccount.prototype, exports.NntpPool.prototype);
  const originals = NAMES.map((n) => exports[n].prototype.fetch);
  guardDurableObjects(exports);
  NAMES.forEach((n, i) => {
    assert.notEqual(exports[n].prototype.fetch, originals[i]);
  });
  assert.notEqual(exports.NntpAccount.prototype.fetch, exports.NntpPool.prototype.fetch);
  for (const n of NAMES) {
    const req = fake("https://a.invalid/x");
    await new exports[n]({}, {}).fetch(req);
    assert.equal(calls[n].length, 1);
    assert.equal(calls[n][0].args[0], req);
    const other = NAMES.find((m) => m !== n);
    assert.equal(calls[other].length, n === NAMES[0] ? 0 : 1);
  }
});

test("255, 256 and 257 unit full URLs on real Requests", async () => {
  const builders = {
    path: (n) => fit("https://a.invalid/", "", n, "p"),
    query: (n) => fit("https://a.invalid/?q=", "", n, "q"),
    authority: hostUrl,
    percent: (n) => fit("https://a.invalid/", "", n, "%C3%A9").padEnd(n, "a"),
  };
  for (const [label, build] of Object.entries(builders)) {
    for (const [n, expected] of [[255, true], [256, true], [257, false]]) {
      const request = new Request(build(n));
      assert.equal(request.url.length, n, `${label} ${n}`);
      assert.equal(await admittedBoth(request), expected, `${label} ${n}`);
    }
  }
  const serialized = new Request("https://a.invalid/" + "é".repeat(10) + "?q=ü€").url;
  assert.ok(serialized.length < 256);
  assert.ok(/^[\x00-\x7f]+$/.test(serialized));
  assert.equal(serialized.includes("%C3%A9"), true);
  assert.equal(await admittedBoth(new Request(serialized)), true);
  const padded = new Request(
    "https://a.invalid/" + "é".repeat(60) + "?q=" + "€".repeat(20),
  );
  assert.ok(padded.url.length > 256);
  assert.equal(await admittedBoth(padded), false);
});

test("modest 8192 unit URL is rejected", async () => {
  const url = fit("https://a.invalid/", "", 8192);
  assert.equal(await admittedBoth(fake(url)), false);
  assert.equal(await admittedBoth(new Request(url)), false);
});

test("UTF-8 byte accounting matches TextEncoder at the bound", async () => {
  const lone = [
    ["lone high", "\ud83d"],
    ["lone low", "\ude00"],
    ["reversed pair", "\ude00\ud83d"],
  ];
  const cases = [
    ["ascii", "a", 1],
    ["two byte", "é", 2],
    ["three byte", "€", 3],
    ["surrogate pair", "😀", 4],
    ...lone.map(([n, s]) => [n, s, 3 * (s.length === 1 ? 1 : 2)]),
  ];
  let checked = 0;
  for (const [label, unit] of cases) {
    for (let head = 0; head < 8; head++) {
      for (const tail of [0, 1, 4]) {
        const url = "a".repeat(256 - head * 4 - tail * 4 - 8) + unit.repeat(head + tail + 2);
        const bytes = new TextEncoder().encode(url).length;
        const expected = bytes <= 256 && url.length <= 256;
        assert.equal(await admittedBoth(fake(url)), expected, `${label} ${bytes}`);
        checked++;
      }
    }
  }
  assert.ok(checked > 100);

  // Exact boundaries for each unit width.
  const edges = [
    ["a", 1], ["é", 2], ["€", 3], ["😀", 4],
    ["\ud83d", 3], ["\ude00", 3],
  ];
  for (const [unit, width] of edges) {
    for (let pad = 0; pad <= 256; pad++) {
      if ((256 - pad) % width !== 0) continue;
      const url = "a".repeat(pad) + unit.repeat((256 - pad) / width);
      assert.equal(new TextEncoder().encode(url).length, 256);
      assert.equal((await admittedBoth(fake(url))), true);
      assert.equal((await admittedBoth(fake(url + "a"))), false);
      break;
    }
  }
  // A high surrogate followed by a non-low unit is lone, and counts as 3.
  const mixed = "a".repeat(253) + "\ud83d";
  assert.equal(new TextEncoder().encode(mixed).length, 256);
  assert.equal(await admittedBoth(fake(mixed)), true);
  const mixed2 = "a".repeat(254) + "\ud83d";
  assert.equal(new TextEncoder().encode(mixed2).length, 257);
  assert.equal(await admittedBoth(fake(mixed2)), false);
  const tailHigh = "a".repeat(252) + "\ud83d" + "b";
  assert.equal(new TextEncoder().encode(tailHigh).length, 256);
  assert.equal(await admittedBoth(fake(tailHigh)), true);
  // Pair at the very end of a 256 byte string counts as four, not six.
  const pairEnd = "a".repeat(252) + "😀";
  assert.equal(await admittedBoth(fake(pairEnd)), true);
  assert.equal(await admittedBoth(fake("a" + pairEnd)), false);
});

test("invalid, missing, non-string and throwing urls get 404 untouched", async () => {
  const touched = [];
  const trap = (target) => new Proxy(target, {
    get(t, key) {
      touched.push(String(key));
      if (key === "url") return t.url;
      throw new Error(`unexpected get ${String(key)}`);
    },
    has() { throw new Error("unexpected has"); },
    ownKeys() { throw new Error("unexpected ownKeys"); },
    getOwnPropertyDescriptor() { throw new Error("unexpected descriptor"); },
    set() { throw new Error("unexpected set"); },
  });
  const throwing = {
    get url() { throw new Error("boom"); },
    clone() { throw new Error("clone"); },
    get headers() { throw new Error("headers"); },
    get body() { throw new Error("body"); },
  };
  const requests = [
    trap({ url: "a".repeat(257) }),
    trap({ url: 42 }),
    trap({ url: null }),
    trap({ url: { toString() { throw new Error("toString"); } } }),
    trap({ url: new String("https://a.invalid/") }),
    trap({}),
    throwing,
    undefined,
    null,
    "https://a.invalid/",
    42,
  ];

  const saved = {};
  const globals = ["TextEncoder", "TextDecoder", "URL", "Request"];
  for (const g of globals) saved[g] = Object.getOwnPropertyDescriptor(globalThis, g);
  const spy = () => { throw new Error("must not be used"); };
  try {
    for (const request of requests) {
      const { exports, calls } = makeExports();
      guardDurableObjects(exports);
      // Install traps after the guard exists; Response must stay usable.
      for (const g of globals) {
        Object.defineProperty(globalThis, g, {
          value: spy, writable: true, configurable: true, enumerable: false,
        });
      }
      for (const n of NAMES) {
        const res = await new exports[n]({}, {}).fetch(request);
        assert.equal(res.status, 404);
        assert.equal(await res.text(), "not found");
        assert.equal(calls[n].length, 0);
      }
      for (const g of globals) Object.defineProperty(globalThis, g, saved[g]);
    }
  } finally {
    for (const g of globals) Object.defineProperty(globalThis, g, saved[g]);
  }
  assert.ok(touched.every((key) => key === "url"), touched.join());
});

test("admitted requests are passed on without touching anything but url", async () => {
  const touched = [];
  const request = new Proxy({ url: "https://a.invalid/ok" }, {
    get(t, key) {
      touched.push(String(key));
      return t[key];
    },
  });
  const { exports, calls } = makeExports();
  guardDurableObjects(exports);
  for (const n of NAMES) await new exports[n]({}, {}).fetch(request);
  assert.deepEqual([...new Set(touched)], ["url"]);
  for (const n of NAMES) assert.equal(calls[n][0].args[0], request);
});

test("descriptor attributes are preserved", () => {
  for (const attrs of [
    { writable: true, configurable: true, enumerable: false },
    { writable: true, configurable: true, enumerable: true },
    { writable: true, configurable: false, enumerable: false },
    { writable: false, configurable: true, enumerable: false },
  ]) {
    const { exports } = makeExports();
    for (const n of NAMES) {
      const proto = exports[n].prototype;
      Object.defineProperty(proto, "fetch", { ...attrs, value: proto.fetch });
    }
    guardDurableObjects(exports);
    for (const n of NAMES) {
      const d = Object.getOwnPropertyDescriptor(exports[n].prototype, "fetch");
      assert.equal(typeof d.value, "function");
      assert.equal(d.writable, attrs.writable);
      assert.equal(d.configurable, attrs.configurable);
      assert.equal(d.enumerable, attrs.enumerable);
      assert.equal("get" in d, false);
    }
  }
});

function invalidFetchClasses() {
  class NoFetch {}
  class Inherited extends makeClass().Fake {}
  class Accessor {}
  Object.defineProperty(Accessor.prototype, "fetch", {
    configurable: true,
    get() { return () => {}; },
  });
  class NotFn {}
  Object.defineProperty(NotFn.prototype, "fetch", { value: 1, writable: true, configurable: true });
  class Frozen {
    fetch() {}
  }
  Object.defineProperty(Frozen.prototype, "fetch", { writable: false, configurable: false });
  return {
    missingFetch: NoFetch,
    inheritedFetch: Inherited,
    accessor: Accessor,
    nonFunction: NotFn,
    frozen: Frozen,
    nonFunctionExport: {},
    undefinedExport: undefined,
    arrow: () => {},
  };
}

test("invalid exports fail before either class is mutated", () => {
  for (const [label, bad] of Object.entries(invalidFetchClasses())) {
    for (const badName of NAMES) {
      const good = makeClass().Fake;
      const goodName = NAMES.find((n) => n !== badName);
      const exports = { [badName]: bad, [goodName]: good };
      const before = good.prototype.fetch;
      const beforeShape = shape(good.prototype);
      assert.throws(() => guardDurableObjects(exports), Error, `${label} ${badName}`);
      assert.equal(good.prototype.fetch, before, `${label} ${badName}`);
      assert.deepEqual(shape(good.prototype), beforeShape);
    }
  }
});

test("missing exports and getter throws fail before mutation", () => {
  const a = makeClass().Fake;
  const before = a.prototype.fetch;
  assert.throws(() => guardDurableObjects({ NntpAccount: a }));
  assert.throws(() => guardDurableObjects({ NntpPool: a }));
  assert.throws(() => guardDurableObjects({}));
  assert.throws(() => guardDurableObjects(Object.create(null)));
  assert.throws(() => guardDurableObjects({
    NntpAccount: a,
    get NntpPool() { throw new Error("getter"); },
  }), /getter/);
  assert.equal(a.prototype.fetch, before);
});

test("shared prototype is refused before mutation", () => {
  const shared = makeClass().Fake;
  const before = shared.prototype.fetch;
  assert.throws(() => guardDurableObjects({ NntpAccount: shared, NntpPool: shared }));
  const alias = function () {};
  alias.prototype = shared.prototype;
  assert.throws(() => guardDurableObjects({ NntpAccount: shared, NntpPool: alias }));
  assert.equal(shared.prototype.fetch, before);
});

test("repeated install does not double wrap", async () => {
  const { exports, calls } = makeExports();
  guardDurableObjects(exports);
  const first = NAMES.map((n) => exports[n].prototype.fetch);
  guardDurableObjects(exports);
  guardDurableObjects(exports);
  NAMES.forEach((n, i) => assert.equal(exports[n].prototype.fetch, first[i]));
  for (const n of NAMES) {
    await new exports[n]({}, {}).fetch(fake("https://a.invalid/"));
    assert.equal(calls[n].length, 1);
  }
});

test("wrapper keeps receiver, request, arguments and return value", async () => {
  const { exports, calls } = makeExports();
  guardDurableObjects(exports);
  for (const n of NAMES) {
    const receiver = Object.create(exports[n].prototype);
    const req = fake("https://a.invalid/r");
    const extra = [{}, "two"];
    await exports[n].prototype.fetch.call(receiver, req, ...extra);
    const call = calls[n][0];
    assert.equal(call.self, receiver);
    assert.equal(call.args.length, 3);
    assert.equal(call.args[0], req);
    assert.equal(call.args[1], extra[0]);
    assert.equal(call.args[2], extra[1]);
  }
});

test("promises, rejections, throws and plain values pass through exactly", async () => {
  const req = fake("https://a.invalid/r");
  const make = (impl) => {
    class C { fetch() { return impl(); } }
    class D { fetch() { return impl(); } }
    const exports = { NntpAccount: C, NntpPool: D };
    guardDurableObjects(exports);
    return exports;
  };

  const promise = Promise.resolve(new Response("x"));
  let e = make(() => promise);
  for (const n of NAMES) assert.equal(new e[n]().fetch(req), promise);

  const failure = new Error("nope");
  const rejected = Promise.reject(failure);
  rejected.catch(() => {});
  e = make(() => rejected);
  for (const n of NAMES) {
    const got = new e[n]().fetch(req);
    assert.equal(got, rejected);
    await assert.rejects(got, (err) => err === failure);
  }

  e = make(() => { throw failure; });
  for (const n of NAMES) {
    assert.throws(() => new e[n]().fetch(req), (err) => err === failure);
  }

  const plain = { value: 1 };
  e = make(() => plain);
  for (const n of NAMES) assert.equal(new e[n]().fetch(req), plain);
  e = make(() => undefined);
  for (const n of NAMES) assert.equal(new e[n]().fetch(req), undefined);
});

test("rejection returns a synchronous 404 Response and skips the original", async () => {
  const { exports, calls } = makeExports();
  guardDurableObjects(exports);
  for (const n of NAMES) {
    const res = new exports[n]({}, {}).fetch(fake("a".repeat(257)));
    assert.ok(res instanceof Response);
    assert.equal(res.status, 404);
    assert.equal(await res.text(), "not found");
    assert.equal(calls[n].length, 0);
  }
});

test("constructors, state, env and other members are unchanged", () => {
  const { exports } = makeExports();
  const classes = NAMES.map((n) => exports[n]);
  const before = classes.map((c) => ({
    proto: shape(c.prototype),
    statics: shape(c),
    keys: Reflect.ownKeys(c.prototype),
    other: c.prototype.other,
    alarm: c.prototype.alarm,
    ctor: c.prototype.constructor,
  }));
  const exportKeys = Reflect.ownKeys(exports);
  guardDurableObjects(exports);
  assert.deepEqual(Reflect.ownKeys(exports), exportKeys);
  classes.forEach((c, i) => {
    assert.equal(exports[NAMES[i]], c);
    assert.equal(c.prototype.constructor, before[i].ctor);
    assert.equal(c.prototype.other, before[i].other);
    assert.equal(c.prototype.alarm, before[i].alarm);
    assert.deepEqual(shape(c), before[i].statics);
    // No new prototype members (RPC helpers or otherwise).
    assert.deepEqual(Reflect.ownKeys(c.prototype), before[i].keys);
    const after = shape(c.prototype);
    for (const key of Object.keys(after)) {
      if (key === "fetch") continue;
      assert.deepEqual(after[key], before[i].proto[key]);
    }
    const state = { id: 1 };
    const env = { binding: 2 };
    const inst = new c(state, env);
    assert.equal(inst.state, state);
    assert.equal(inst.env, env);
    assert.equal(inst.alarm(), "alarm");
    assert.equal(inst.other(7), 7);
    assert.deepEqual(Object.keys(inst).sort(), ["env", "state"]);
  });
});
