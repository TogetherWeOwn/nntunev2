// Local-only Cloudflare boundary double for runtime.test.mjs. No implementation
// of alert/window logic lives here. The three pure dependencies execute unchanged;
// wrappers record their actual inputs/results so integration cannot fake reuse.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import vm from 'node:vm';
import { projectTailEvent } from '../project-tail-event.mjs';
import { evaluateErrorWindow } from '../evaluate-error-window.mjs';
import { renderVerdictAlert } from '../render-verdict-alert.mjs';

export const START = Date.UTC(2031, 0, 1);
export const plain = (value) => value === undefined ? undefined : structuredClone(value);
const bytes = (value) => Buffer.byteLength(JSON.stringify(value), 'utf8');

export async function settle(promise, message = 'local operation did not settle') {
  let finished = false;
  let result;
  let failure;
  Promise.resolve(promise).then((value) => { result = value; finished = true; },
    (error) => { failure = error; finished = true; });
  for (let i = 0; i < 5000 && !finished; i += 1) await Promise.resolve();
  assert.equal(finished, true, message);
  if (failure) throw failure;
  return result;
}

export class Clock {
  constructor(now = START) { this.now = now; this.timers = new Map(); this.nextId = 1; }
  setTimeout = (callback, delay = 0, ...args) => {
    const id = this.nextId++;
    this.timers.set(id, { at: this.now + Number(delay), callback: () => callback(...args) });
    return id;
  };
  clearTimeout = (id) => { this.timers.delete(id); };
  async tick(ms) {
    const end = this.now + ms;
    for (let i = 0; i < 1000; i += 1) {
      const due = [...this.timers].filter(([, timer]) => timer.at <= end)
        .sort((a, b) => a[1].at - b[1].at)[0];
      if (!due) { this.now = end; return; }
      this.now = due[1].at;
      this.timers.delete(due[0]);
      due[1].callback();
      for (let j = 0; j < 30; j += 1) await Promise.resolve();
    }
    assert.fail('runtime scheduled an unbounded local timer loop');
  }
}

// Storage returns independent structured clones, including inside transactions.
// Transaction changes/alarm updates are atomic and rolled back on rejection.
// Direct I/O is synchronous-before-resolution, as with the DO storage input gate.
export class Storage {
  constructor(clock) {
    this.clock = clock;
    this.data = new Map();
    this.alarm = null;
    this.history = [];
    this.accesses = [];
    this.openTransactions = 0;
    this.lock = Promise.resolve();
  }
  snapshot() { return { values: [...this.data].map(([key, value]) => [key, plain(value)]), alarm: this.alarm }; }
  record() { this.history.push(this.snapshot()); }
  view(data, alarmBox, record) {
    const validate = (value) => {
      assert.ok(bytes(value) <= 65536, 'each durable storage value must be <=64KiB serialized');
    };
    return {
      get: async (key) => {
        this.accesses.push('get');
        return Array.isArray(key)
          ? new Map(key.filter((k) => data.has(k)).map((k) => [k, plain(data.get(k))]))
          : plain(data.get(key));
      },
      put: async (key, value) => {
        const entries = typeof key === 'string' ? [[key, value]] : Object.entries(key);
        for (const [, item] of entries) validate(item);
        for (const [name, item] of entries) data.set(name, plain(item));
        record();
      },
      delete: async (key) => {
        const keys = Array.isArray(key) ? key : [key];
        let deleted = 0;
        for (const name of keys) if (data.delete(name)) deleted += 1;
        record();
        return Array.isArray(key) ? deleted : deleted > 0;
      },
      deleteAll: async () => { data.clear(); record(); },
      list: async (options = {}) => {
        this.accesses.push('list');
        let entries = [...data].filter(([key]) =>
          (options.prefix === undefined || key.startsWith(options.prefix))
          && (options.start === undefined || key >= options.start)
          && (options.startAfter === undefined || key > options.startAfter)
          && (options.end === undefined || key < options.end))
          .sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0);
        if (options.reverse) entries.reverse();
        if (options.limit !== undefined) entries = entries.slice(0, options.limit);
        return new Map(entries.map(([key, value]) => [key, plain(value)]));
      },
      getAlarm: async () => { this.accesses.push('getAlarm'); return alarmBox.get(); },
      setAlarm: async (time) => {
        const value = Number(time);
        assert.ok(Number.isFinite(value), 'alarm deadline must be finite');
        alarmBox.set(value); record();
      },
      deleteAlarm: async () => { alarmBox.set(null); record(); },
      sync: async () => {},
    };
  }
  api() {
    return {
      ...this.view(this.data, { get: () => this.alarm, set: (value) => { this.alarm = value; } }, () => this.record()),
      transaction: async (callback) => {
        const previous = this.lock;
        let release;
        this.lock = new Promise((resolve) => { release = resolve; });
        await previous;
        const copy = new Map([...this.data].map(([key, value]) => [key, plain(value)]));
        let alarm = this.alarm;
        this.openTransactions += 1;
        try {
          const value = await callback(this.view(copy, { get: () => alarm, set: (time) => { alarm = time; } }, () => {}));
          // Keep the Map identity: outstanding nontransactional views see commits.
          this.data.clear();
          for (const [key, item] of copy) this.data.set(key, item);
          this.alarm = alarm;
          this.record();
          return value;
        } finally { this.openTransactions -= 1; release(); }
      },
    };
  }
}

export function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

// An endless response forces the consumer to cancel or cap reads. Calling
// text/json/arrayBuffer/blob is deliberately forbidden: those are unbounded.
export function response(status = 200, headers = {}) {
  const audit = { readBytes: 0, cancelled: false };
  const body = new ReadableStream({
    pull(controller) {
      const chunk = new TextEncoder().encode('SYNTHETIC_PRIVATE_2837 SYNTHETIC_STACK_2837 '.repeat(20));
      audit.readBytes += chunk.byteLength; controller.enqueue(chunk);
    },
    cancel() { audit.cancelled = true; },
  }, { highWaterMark: 0 });
  const value = new Response(body, { status, headers });
  for (const name of ['text', 'json', 'arrayBuffer', 'blob', 'formData']) {
    Object.defineProperty(value, name, { value: async () => assert.fail('response body must be consumed/cancelled boundedly') });
  }
  return { value, audit };
}

export async function harness({ now = START, env: overrides = {}, deliver } = {}) {
  const clock = new Clock(now);
  const storage = new Storage(clock);
  const calls = { project: [], evaluate: [], render: [], binding: [], rpc: [], fetch: [], console: [], responses: [] };
  const waits = [];
  const initializers = [];
  const env = {
    PRODUCER_SCRIPT_NAME: 'nntune-api-staging',
    LOGS_URL: 'https://logs.example.invalid/view/17',
    // Deliberately synthetic short token, assembled only for the fake transport.
    OPS_ALERT_WEBHOOK_URL: `https://discord.com/api/webhooks/123/${'synthetic-local-only'}`,
    BUILD_SHA: 'cccccccccccccccccccccccccccccccccccccccc',
    ...overrides,
  };
  const ctx = {
    storage: storage.api(),
    id: { toString: () => 'local-only-object' },
    waitUntil(promise) { waits.push(Promise.resolve(promise)); },
    blockConcurrencyWhile(callback) {
      const promise = Promise.resolve().then(callback);
      initializers.push(promise);
      return promise;
    },
  };
  class DurableObject {
    constructor(state, bindings) { this.ctx = state; this.env = bindings; }
  }
  const binding = {
    idFromName(name) { calls.binding.push(name); return name; },
    get(id) { assert.equal(id, 'api:staging', 'fixed staging aggregation object'); return stub; },
    getByName(name) { calls.binding.push(name); assert.equal(name, 'api:staging'); return stub; },
  };
  const stub = {
    async ingest(input) { const copy = plain(input); calls.rpc.push(plain(copy)); return instance.ingest(copy); },
  };
  env.ALERT_WINDOWS = binding;
  const fetch = async (input, init = {}) => {
    assert.equal(storage.openTransactions, 0, 'external I/O must not hold a storage transaction');
    const durable = storage.snapshot();
    const at = clock.now;
    const request = input instanceof Request ? input : new Request(input, init);
    const call = {
      url: request.url, method: request.method, redirect: request.redirect,
      headers: Object.fromEntries(request.headers), signal: request.signal,
      body: await request.text(), at, durable,
    };
    calls.fetch.push(call);
    if (deliver) return deliver(call);
    const reply = response();
    calls.responses.push(reply.audit);
    return reply.value;
  };
  class FakeDate extends Date {
    constructor(...args) { super(...(args.length ? args : [clock.now])); }
    static now() { return clock.now; }
  }
  const abortSignal = new Proxy(AbortSignal, {
    get(target, key) {
      if (key === 'timeout') return (ms) => {
        const controller = new AbortController();
        clock.setTimeout(() => controller.abort(new DOMException('local deadline', 'TimeoutError')), ms);
        return controller.signal;
      };
      const value = Reflect.get(target, key);
      return typeof value === 'function' ? value.bind(target) : value;
    },
  });
  async function loadNamespace() {
    // Fresh VM/module globals on reconstruction: a module-global cooldown or
    // in-memory consumed flag must not masquerade as durable persistence.
    const sandbox = vm.createContext({
      Date: FakeDate, TextEncoder, TextDecoder, URL, URLSearchParams,
      Request, Response, Headers, AbortController, AbortSignal: abortSignal, DOMException,
      ReadableStream, WritableStream, TransformStream, structuredClone,
      fetch, setTimeout: clock.setTimeout, clearTimeout: clock.clearTimeout, queueMicrotask,
      console: Object.fromEntries(['log', 'info', 'warn', 'error', 'debug', 'trace', 'dir', 'assert']
        .map((key) => [key, (...args) => { calls.console.push(args.map(String)); }])),
    });
    const real = {
      'project-tail-event.mjs': ['projectTailEvent', projectTailEvent, 'project'],
      'evaluate-error-window.mjs': ['evaluateErrorWindow', evaluateErrorWindow, 'evaluate'],
      'render-verdict-alert.mjs': ['renderVerdictAlert', renderVerdictAlert, 'render'],
    };
    const root = new URL('../', import.meta.url);
    const cache = new Map();
    async function load(url) {
      if (cache.has(url.href)) return cache.get(url.href);
      assert.ok(url.href.startsWith(root.href), 'runtime imports must remain local to observability');
      const definition = real[url.pathname.split('/').at(-1)];
      if (definition && url.href === new URL(url.pathname.split('/').at(-1), root).href) {
        const [name, fn, bucket] = definition;
        const module = new vm.SyntheticModule([name], function () {
          this.setExport(name, (...args) => {
            const result = fn(...args);
            calls[bucket].push({ args: plain(args), result: plain(result) });
            return result;
          });
        }, { context: sandbox, identifier: url.href });
        cache.set(url.href, module);
        return module;
      }
      const module = new vm.SourceTextModule(await readFile(url, 'utf8'), {
        context: sandbox, identifier: url.href,
        initializeImportMeta(meta) { meta.url = url.href; },
        importModuleDynamically() { assert.fail('runtime must not load ambient/network modules'); },
      });
      cache.set(url.href, module);
      return module;
    }
    let namespace;
    try {
      const module = await load(new URL('runtime.mjs', root));
      await module.link(async (specifier, parent) => {
        if (specifier === 'cloudflare:workers') return new vm.SyntheticModule(['DurableObject'], function () {
          this.setExport('DurableObject', DurableObject);
        }, { context: sandbox });
        assert.match(specifier, /^\.\.?\//, 'runtime may only import real local helpers and cloudflare:workers');
        return load(new URL(specifier, parent.identifier));
      });
      await module.evaluate({ timeout: 1000 });
      namespace = module.namespace;
    } catch (error) {
      if (error.code === 'ENOENT') assert.fail('missing runtime contract: workers/observability/runtime.mjs must connect Tail, durable windows and delivery');
      throw error;
    }
    assert.equal(typeof namespace.default?.tail, 'function', 'default.tail contract missing');
    assert.equal(typeof namespace.default?.fetch, 'function', 'public fetch refusal contract missing');
    assert.equal(typeof namespace.ErrorRateAlerts, 'function', 'named ErrorRateAlerts RPC/alarm class missing');
    assert.ok(namespace.ErrorRateAlerts.prototype instanceof DurableObject, 'ErrorRateAlerts must extend cloudflare:workers DurableObject');
    return namespace;
  }
  let namespace = await loadNamespace();
  let instance;
  async function boot() {
    instance = new namespace.ErrorRateAlerts(ctx, env);
    await settle(Promise.all(initializers.splice(0)), 'DO initialization did not settle');
  }
  await boot();
  async function drain() {
    for (let i = 0; i < 10 && waits.length; i += 1) await settle(Promise.all(waits.splice(0)));
    assert.equal(waits.length, 0, 'unbounded waitUntil work');
  }
  return {
    env, ctx, clock, storage, calls,
    get namespace() { return namespace; },
    async restart() { namespace = await loadNamespace(); await boot(); },
    async tail(events) { await settle(namespace.default.tail(events, env, ctx)); await drain(); },
    async ingest(input) { return settle(instance.ingest(input)); },
    alarm() { return Promise.resolve().then(() => instance.alarm()).then(async (result) => { await drain(); return result; }); },
    async close(at = clock.now + 60000) { clock.now = at; await settle(this.alarm(), 'alarm did not finish boundedly'); },
    async publicFetch(request) { return settle(namespace.default.fetch(request, env, ctx)); },
  };
}
