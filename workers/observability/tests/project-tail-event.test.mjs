// Independently owned acceptance contract. Builder must not edit this file.
// Import: named synchronous projectTailEvent(event, policy) from ../project-tail-event.mjs.
// Command (repo root):
// node --experimental-vm-modules --test workers/observability/tests/project-tail-event.test.mjs
//
// The tests exercise the actual ESM in an isolated VM, with imports, ambient I/O,
// logging, clocks, random values, timers and environment access forbidden. This
// is a dependency-free projector, not a Tail consumer or an alerting deployment.
// No cross-delivery deduplication is promised: each call is one invocation.
//
// Frozen v1 schema (no additional fields; http_status is omitted when unknown):
// {schema_version: 1, service: 'api', environment: 'staging', outcome:
//  'ok'|'exception'|'exceeded_cpu'|'exceeded_memory'|'unknown', http_status?: 100..599,
//  failure_flags: {http_5xx: boolean, exception: boolean, exceeded_cpu: boolean,
//                  exceeded_memory: boolean},
//  build_sha: lowercase40hex|'unknown', release: lowercase40hex|'unknown',
//  route: allowlisted|'unknown', error_class: allowlisted|'unknown'}
// All returned objects are synchronous JSON data, never a batch or Promise.
// Invalid policy, malformed root event or nonmatching producer => null, no throw.
// Unknown outcome/status is not evidence of health (or of an HTTP 500).
//
// Source status is Tail invocation.event.response.status, never request.status,
// top-level response/status, or a log field. Non-fetch/missing response omits it.
// Identity/grouping comes only from JSON strings in logs[].message[] containing
// the API's EXACT six-key served envelope. Additional source keys are untrusted.
// Every matching served record must have equal valid build_sha/release. One bad
// pair or any conflict poisons BOTH identities. Repeated equal pairs are fine.
// Group values are exact allowlist matches (no trim/case fold/path derivation);
// a missing, invalid or conflicting value produces 'unknown' for that group.
//
// Bounds: <=64 logs, <=4 messages/log, <=16384 UTF-8 bytes/string BEFORE parsing.
// Any truncated/oversized scan or malformed logs/message container makes ALL
// identity/grouping unknown, even after an early valid record. Malformed JSON
// and non-string messages within valid containers are ignored. Platform outcome,
// exceptions and HTTP status remain usable independently of all log scanning.
// Empty/missing logs are a complete scan with no identity. Exceptions are flagged
// by a nonempty array or outcome='exception', never by log level or error text.
//
// Policy: {script_name: 'nntune-api-staging', service: 'api', environment: 'staging',
//          routes: string[], error_classes: string[]} — all five are required.
// This v1 slice accepts ONLY that explicit producer/label triple (deploy.yml's
// staging API). Each allowlist is finite (0..64 entries). Routes are canonical
// uppercase HTTP method + space + safe absolute path/template, <=128 characters;
// no URL, query, fragment, whitespace/control or dynamic identifier expansion.
// Error classes match [a-z][a-z0-9_]{0,63}. No coercion or policy defaults.
// Trusted source envelope keys: ts, level, worker, trace_id, event, fields;
// worker='api', event='api.request.served', fields is flat scalar/null data.
// See docs/logging-standard.md; source identity is fields.build_sha/release.
// The complete valid served-envelope schema (including ts/level/trace_id) is
// required. Unknown extra fields INSIDE fields are never copied to output.
// Tests only: no source placeholder; the Builder adds the module, README and CI.

import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import vm from 'node:vm';

const MODULE_URL = new URL('../project-tail-event.mjs', import.meta.url);
const SHA_A = '0123456789abcdef0123456789abcdef01234567';
const SHA_B = 'fedcba9876543210fedcba9876543210fedcba98';
// All canaries are deliberately synthetic, never credentials or real user data.
const CANARIES = [
  'SYNTHETIC_SECRET_NEVER_EMIT_713',
  'synthetic-person-713@example.invalid',
  'synthetic-tenant-713',
  'SYNTHETIC_COOKIE_NEVER_EMIT_713',
  'SYNTHETIC_STACK_NEVER_EMIT_713',
];
const FORBIDDEN = [];
const PARSED_TEXTS = [];
const deny = (name) => (..._args) => {
  FORBIDDEN.push(name);
  throw new Error(`projector attempted forbidden ambient access: ${name}`);
};
const math = Object.create(Math);
math.random = deny('Math.random');
const context = vm.createContext({
  console: Object.fromEntries(['log', 'info', 'warn', 'error', 'debug', 'trace', 'dir', 'assert']
    .map((name) => [name, deny(`console.${name}`)])),
  fetch: deny('fetch'),
  WebSocket: deny('WebSocket'),
  XMLHttpRequest: deny('XMLHttpRequest'),
  Date: new Proxy(Date, {
    apply: deny('Date()'),
    construct(target, args) {
      if (args.length === 0) return deny('new Date()')();
      return Reflect.construct(target, args);
    },
    get(target, key) { return key === 'now' ? deny('Date.now') : Reflect.get(target, key); },
  }),
  JSON: {
    parse(text, ...args) {
      PARSED_TEXTS.push(text);
      if (typeof text === 'string' && Buffer.byteLength(text, 'utf8') > 16384) {
        return deny('oversized JSON.parse')();
      }
      return JSON.parse(text, ...args);
    },
    stringify: JSON.stringify,
  },
  performance: new Proxy({}, { get: deny('performance') }),
  process: new Proxy({}, { get: deny('process/environment') }),
  crypto: new Proxy({}, { get: deny('crypto/random') }),
  Math: math,
  setTimeout: deny('setTimeout'), setInterval: deny('setInterval'),
  setImmediate: deny('setImmediate'), queueMicrotask: deny('queueMicrotask'),
  TextEncoder, TextDecoder,
});
let namespace;
let loadFailure;
let lastRawResult;
try {
  const source = await readFile(MODULE_URL, 'utf8');
  const module = new vm.SourceTextModule(source, {
    context,
    identifier: MODULE_URL.href,
    importModuleDynamically: deny('dynamic import'),
  });
  await module.link(deny('static import'));
  await module.evaluate({ timeout: 1000 });
  namespace = module.namespace;
} catch (error) {
  // Deliberately no fake implementation or source placeholder. Missing behavior
  // becomes an assertion failure, not an uncaught module-resolution exception.
  loadFailure = error.code === 'ENOENT' ? 'missing projector module' : 'projector did not load safely';
}

function project(event, ...policies) {
  const policy = policies.length === 0 ? policyFixture() : policies[0];
  assert.equal(typeof namespace?.projectTailEvent, 'function', loadFailure ?? 'missing named export');
  const before = FORBIDDEN.length;
  let result;
  let thrown;
  try { result = namespace.projectTailEvent(event, policy); } catch (error) { thrown = error; }
  if (thrown) assertNoCanaries(String(thrown) + String(thrown.stack));
  assert.equal(FORBIDDEN.length, before, 'projection attempted a forbidden side effect');
  assert.equal(thrown, undefined, 'JSON-compatible input must not throw');
  assert.notEqual(result, undefined, 'projection must return an observation or null');
  assert.equal(typeof result?.then, 'undefined', 'projection must be synchronous');
  assertNoCanaries(JSON.stringify(result));
  lastRawResult = result;
  // Normalize only the cross-realm prototypes, not the production field values.
  return result === null ? null : JSON.parse(JSON.stringify(result));
}

function assertNoCanaries(text) {
  for (const canary of CANARIES) {
    assert.equal(text.includes(canary), false, 'projector leaked a synthetic secret/PII canary');
  }
}

function policyFixture(overrides = {}) {
  return {
    script_name: 'nntune-api-staging', service: 'api', environment: 'staging',
    routes: ['GET /healthz', 'GET /v1/library/search', 'DELETE /v1/credentials/{id}'],
    error_classes: ['internal_error', 'dependency_unavailable'],
    ...overrides,
  };
}

function served(fields = {}, envelope = {}) {
  return JSON.stringify({
    ts: '2026-10-22T00:00:00.000Z', level: 'info', worker: 'api',
    event: 'api.request.served', trace_id: '9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c',
    fields: {
      build_sha: SHA_A, release: SHA_A, route: 'GET /v1/library/search',
      status_code: 200, duration_ms: 4, ...fields,
    },
    ...envelope,
  });
}

function invocation(overrides = {}) {
  return {
    scriptName: 'nntune-api-staging', outcome: 'ok',
    eventTimestamp: 1792627200000,
    event: {
      request: { url: 'https://example.invalid/v1/library/search?q=synthetic', method: 'GET', headers: {} },
      response: { status: 200 },
    },
    logs: [{ timestamp: 1792627200000, level: 'log', message: [served()] }],
    exceptions: [],
    ...overrides,
  };
}

function expected(overrides = {}) {
  return {
    schema_version: 1, service: 'api', environment: 'staging', outcome: 'ok', http_status: 200,
    failure_flags: { http_5xx: false, exception: false, exceeded_cpu: false, exceeded_memory: false },
    build_sha: SHA_A, release: SHA_A, route: 'GET /v1/library/search', error_class: 'unknown',
    ...overrides,
  };
}

function withMessages(messages, overrides = {}) {
  return invocation({ logs: [{ timestamp: 1792627200000, level: 'log', message: messages }], ...overrides });
}

function assertUnknownIdentity(result) {
  assert.notEqual(result, null);
  assert.equal(result.build_sha, 'unknown');
  assert.equal(result.release, 'unknown');
}

function assertUnknownMetadata(result) {
  assertUnknownIdentity(result);
  assert.equal(result.route, 'unknown');
  assert.equal(result.error_class, 'unknown');
}

function deepFreeze(value) {
  if (value && typeof value === 'object') {
    for (const child of Object.values(value)) deepFreeze(child);
    Object.freeze(value);
  }
  return value;
}

// Break caught: a missing export, import-time side effect, or dependency bypass.
test('loads the named projector export without ambient effects or dependencies', () => {
  assert.equal(loadFailure, undefined, 'implement project-tail-event.mjs with the frozen named export');
  assert.equal(typeof namespace?.projectTailEvent, 'function');
  assert.deepEqual(FORBIDDEN, []);
});

// Break caught: a failure-only projector loses the future error-rate denominator.
test('healthy invocation is one exact-schema observation with validated source identity', () => {
  assert.deepEqual(project(invocation()), expected());
});

// Break caught: suffix guessing, production acceptance, or trusting event labels.
for (const scriptName of ['nntune-api', 'nntune-api-production', 'other-api-staging',
  'nntune-api-staging-extra', 'https://example.invalid/nntune-api-staging', '', null, 42]) {
  test(`rejects unlisted producer (${JSON.stringify(scriptName)})`, () => {
    assert.equal(project(invocation({ scriptName, service: 'api', environment: 'staging' })), null);
  });
}
test('does not infer the producer from a request URL or application log', () => {
  const event = invocation();
  delete event.scriptName;
  assert.equal(project(event), null);
});

const invalidPolicies = [
  undefined, null, [], 'staging', {},
  ...['script_name', 'service', 'environment', 'routes', 'error_classes'].map((key) => {
    const policy = policyFixture(); delete policy[key]; return policy;
  }),
  policyFixture({ script_name: 'nntune-api' }),
  policyFixture({ script_name: 'https://example.invalid/api-staging' }),
  policyFixture({ script_name: ['nntune-api-staging'] }),
  policyFixture({ service: 'observer' }), policyFixture({ environment: 'production' }),
  policyFixture({ environment: 'STAGING' }),
  policyFixture({ routes: '*' }), policyFixture({ error_classes: '*' }),
  policyFixture({ routes: [null] }), policyFixture({ error_classes: [42] }),
  policyFixture({ routes: ['https://example.invalid/v1/library/search'] }),
  policyFixture({ routes: ['GET /v1/library/search?secret=synthetic'] }),
  policyFixture({ routes: ['GET /healthz\n'] }),
  policyFixture({ routes: ['GET /' + 'a'.repeat(128)] }),
  policyFixture({ error_classes: ['internal_error\n'] }),
  policyFixture({ error_classes: ['a'.repeat(65)] }),
  policyFixture({ routes: Array.from({ length: 65 }, (_, i) => `GET /route${i}`) }),
  policyFixture({ error_classes: Array.from({ length: 65 }, (_, i) => `error_${i}`) }),
  policyFixture({ environment: CANARIES[0] }),
];
for (const [index, policy] of invalidPolicies.entries()) {
  test(`invalid policy fails closed without diagnostics (${index})`, () => {
    assert.equal(project(invocation(), policy), null);
  });
}
test('empty allowlists retain the observation but discard grouping', () => {
  assert.deepEqual(project(invocation(), policyFixture({ routes: [], error_classes: [] })),
    expected({ route: 'unknown' }));
});
test('allowlists at the exact finite cap remain valid', () => {
  const policy = policyFixture({
    routes: ['GET /v1/library/search', ...Array.from({ length: 63 }, (_, i) => `GET /route${i}`)],
    error_classes: ['internal_error', ...Array.from({ length: 63 }, (_, i) => `error_${i}`)],
  });
  assert.deepEqual(project(withMessages([served({ error_class: 'internal_error' })]), policy),
    expected({ error_class: 'internal_error' }));
});
test('producer policy is read on each call, not cached from an earlier invocation', () => {
  assert.deepEqual(project(invocation()), expected());
  assert.equal(project(invocation(), policyFixture({ environment: 'production' })), null);
});
for (const [index, event] of [null, [], 'raw input', 42, true, {}].entries()) {
  test(`malformed root event fails closed (${index})`, () => assert.equal(project(event), null));
}

// Break caught: conflating outcome and status, or emitting separate observations
// for each flag on one invocation. Expectations are independent literal tables.
const outcomes = [
  ['ok', 200, [], 'ok', [false, false, false, false]],
  ['ok', 204, [], 'ok', [false, false, false, false]],
  ['ok', 404, [], 'ok', [false, false, false, false]],
  ['ok', 429, [], 'ok', [false, false, false, false]],
  ['ok', 500, [], 'ok', [true, false, false, false]],
  ['ok', 599, [], 'ok', [true, false, false, false]],
  ['exception', 200, [], 'exception', [false, true, false, false]],
  ['ok', 200, [{ name: 'Error', message: CANARIES[0], stack: CANARIES[4] }], 'ok', [false, true, false, false]],
  ['ok', 200, [null], 'ok', [false, true, false, false]],
  ['ok', 503, [{ message: CANARIES[0] }], 'ok', [true, true, false, false]],
  ['exceededCpu', 200, [], 'exceeded_cpu', [false, false, true, false]],
  ['exceededMemory', 200, [], 'exceeded_memory', [false, false, false, true]],
  ['exceededCpu', 503, [{ message: CANARIES[0] }], 'exceeded_cpu', [true, true, true, false]],
  ['exceededMemory', 503, [{ message: CANARIES[0] }], 'exceeded_memory', [true, true, false, true]],
  ['futurePlatformOutcome', 200, [], 'unknown', [false, false, false, false]],
  ['unknown', 503, [], 'unknown', [true, false, false, false]],
  ['canceled', 200, [], 'unknown', [false, false, false, false]],
  ['scriptNotFound', 200, [], 'unknown', [false, false, false, false]],
  ['responseStreamDisconnected', 200, [], 'unknown', [false, false, false, false]],
  [null, 200, [], 'unknown', [false, false, false, false]],
];
for (const [index, [outcome, status, exceptions, normalized, flags]] of outcomes.entries()) {
  test(`one invocation keeps independent failure flags (${index})`, () => {
    const [http_5xx, exception, exceeded_cpu, exceeded_memory] = flags;
    assert.deepEqual(project(invocation({ outcome, exceptions, event: { response: { status } } })),
      expected({ outcome: normalized, http_status: status,
        failure_flags: { http_5xx, exception, exceeded_cpu, exceeded_memory } }));
  });
}
test('all exceptions still produce one observation and repeated delivery is not deduplicated', () => {
  const event = invocation({ exceptions: [{ message: CANARIES[0] }, { message: CANARIES[4] }] });
  const want = expected({ failure_flags: {
    http_5xx: false, exception: true, exceeded_cpu: false, exceeded_memory: false,
  } });
  assert.deepEqual(project(event), want);
  assert.deepEqual(project(event), want);
});
test('error-level ordinary logs are not platform exceptions', () => {
  assert.deepEqual(project(invocation({ logs: [{ level: 'error', message: [served()] }] })), expected());
});
for (const exceptions of [null, {}, 'exception', 42]) {
  test(`non-array exceptions are ignored safely (${JSON.stringify(exceptions)})`, () => {
    assert.deepEqual(project(invocation({ exceptions })), expected());
  });
}

// Break caught: using a log status or top-level response instead of FetchEventInfo.
for (const status of [100, 199, 299, 400, 499]) {
  test(`valid non-5xx status is preserved (${status})`, () => {
    assert.deepEqual(project(invocation({ event: { response: { status } } })), expected({ http_status: status }));
  });
}
for (const [index, status] of [null, '500', 99, 600, 500.5, {}, true].entries()) {
  test(`invalid HTTP status stays absent, never fabricated (${index})`, () => {
    const want = expected(); delete want.http_status;
    assert.deepEqual(project(invocation({ event: { response: { status } } })), want);
  });
}
for (const [index, event] of [null, {}, { cron: '* * * * *' }, { response: null },
  { response: {} }, { request: { status: 500 } }].entries()) {
  test(`non-fetch or missing response has no HTTP status (${index})`, () => {
    const want = expected(); delete want.http_status;
    assert.deepEqual(project(invocation({ event, status: 500, response: { status: 500 } })), want);
  });
}
test('unknown outcome and absent status remain explicitly unknown', () => {
  const event = invocation({ event: null }); delete event.outcome;
  const want = expected({ outcome: 'unknown' }); delete want.http_status;
  assert.deepEqual(project(event), want);
});
test('Tail status wins over conflicting log, request and top-level status', () => {
  const event = withMessages([served({ status_code: 500 })], {
    event: { request: { status: 500 }, response: { status: 204 } },
    response: { status: 500 }, status: 500,
  });
  assert.deepEqual(project(event), expected({ http_status: 204 }));
});
test('exception before served logging survives with unknown release and no response', () => {
  const event = invocation({ outcome: 'exception', event: null, logs: [],
    exceptions: [{ name: 'Error', message: CANARIES[0], stack: CANARIES[4] }] });
  const want = expected({ outcome: 'exception', build_sha: 'unknown', release: 'unknown',
    route: 'unknown', error_class: 'unknown', failure_flags: {
      http_5xx: false, exception: true, exceeded_cpu: false, exceeded_memory: false,
    } });
  delete want.http_status;
  assert.deepEqual(project(event), want);
});

// Break caught: cherry-picking identity, coercing/normalizing SHA, or trusting
// a seventh top-level field / the observer's build instead of producer fields.
for (const [index, fields] of [
  { build_sha: null }, { release: null }, { build_sha: 'unknown', release: 'unknown' },
  { build_sha: SHA_A.toUpperCase(), release: SHA_A.toUpperCase() },
  { build_sha: SHA_A.slice(0, 39), release: SHA_A.slice(0, 39) },
  { build_sha: SHA_A + '0', release: SHA_A + '0' },
  { build_sha: ' ' + SHA_A, release: ' ' + SHA_A },
  { build_sha: SHA_A + '\n', release: SHA_A + '\n' },
  { build_sha: 'g'.repeat(40), release: 'g'.repeat(40) },
  { build_sha: 42, release: 42 }, { build_sha: [SHA_A], release: [SHA_A] },
  { build_sha: SHA_A, release: SHA_B },
].entries()) {
  test(`invalid or mismatched identity is unknown for both fields (${index})`, () => {
    assertUnknownIdentity(project(withMessages([served(fields)])));
  });
}
for (const key of ['build_sha', 'release']) {
  test(`missing ${key} cannot borrow its paired identity`, () => {
    const record = JSON.parse(served()); delete record.fields[key];
    assertUnknownIdentity(project(withMessages([JSON.stringify(record)])));
  });
}
test('same valid identity across records remains known', () => {
  assert.deepEqual(project(withMessages([served(), served()])), expected());
});
test('a different valid producer release replaces prior call identity rather than using observer state', () => {
  assert.deepEqual(project(invocation()), expected());
  assert.deepEqual(project(withMessages([served({ build_sha: SHA_B, release: SHA_B })])),
    expected({ build_sha: SHA_B, release: SHA_B }));
});
for (const [index, messages] of [
  [served(), served({ build_sha: SHA_B, release: SHA_B })],
  [served({ build_sha: SHA_B, release: SHA_B }), served()],
  [served(), served({ release: 'unknown' })],
  [served({ build_sha: null }), served()],
].entries()) {
  test(`conflicting/invalid served identity cannot be hidden by order (${index})`, () => {
    assertUnknownIdentity(project(withMessages(messages)));
  });
}
for (const [index, envelope] of [
  { worker: 'ingest' }, { event: 'api.auth.login' },
  { build_sha: SHA_B }, { release: SHA_B }, { service: 'api' },
  { trace_id: 'not-a-trace' }, { trace_id: 'A'.repeat(32) }, { level: 'verbose' },
  { ts: null }, { ts: 'not-a-timestamp' }, { fields: [] },
].entries()) {
  test(`nonconforming served envelope cannot supply identity (${index})`, () => {
    assertUnknownMetadata(project(withMessages([served({}, envelope)])));
  });
}
test('an API error-level served record retains source identity and allowlisted error class', () => {
  const event = withMessages([served({ status_code: 500, error_class: 'internal_error' }, { level: 'error' })],
    { event: { response: { status: 500 } } });
  assert.deepEqual(project(event), expected({ http_status: 500, error_class: 'internal_error',
    failure_flags: { http_5xx: true, exception: false, exceeded_cpu: false, exceeded_memory: false } }));
});
test('service spelling is not an alias for the six-key worker field', () => {
  const record = JSON.parse(served()); record.service = record.worker; delete record.worker;
  assertUnknownMetadata(project(withMessages([JSON.stringify(record)])));
});
test('unrelated worker/event records cannot poison a real API served identity', () => {
  assert.deepEqual(project(withMessages([
    served({ build_sha: SHA_B, release: SHA_B }, { worker: 'ingest' }),
    served({ build_sha: SHA_B, release: SHA_B }, { event: 'api.auth.login' }), served(),
  ])), expected());
});
test('identity is not sourced from root fields, policy, direct log objects or JSON arrays', () => {
  const record = JSON.parse(served());
  const event = withMessages([record, JSON.stringify([record])], { build_sha: SHA_B, release: SHA_B });
  assertUnknownMetadata(project(event, { ...policyFixture(), build_sha: SHA_B, release: SHA_B }));
});
test('a seventh-key served record does not become trusted just because a valid record came first', () => {
  assertUnknownIdentity(project(withMessages([served(), served({}, { release: SHA_B })])));
});

// Break caught: leaking dynamic paths/errors instead of exact finite group labels.
for (const route of ['GET /healthz', 'DELETE /v1/credentials/{id}']) {
  test(`canonical allowlisted route passes (${route})`, () => {
    assert.deepEqual(project(withMessages([served({ route })])), expected({ route }));
  });
}
for (const error_class of ['internal_error', 'dependency_unavailable']) {
  test(`allowlisted error class passes (${error_class})`, () => {
    assert.deepEqual(project(withMessages([served({ error_class })])), expected({ error_class }));
  });
}
for (const [index, route] of [null, 42, ['GET /healthz'], 'get /healthz', ' GET /healthz',
  'GET /healthz\n', 'GET /healthz?token=synthetic', 'GET /v1/credentials/synthetic-tenant-713',
  'https://example.invalid/healthz', CANARIES[1]].entries()) {
  test(`untrusted route uses a fixed sentinel (${index})`, () => {
    assert.equal(project(withMessages([served({ route })])).route, 'unknown');
  });
}
for (const [index, error_class] of [null, 42, ['internal_error'], 'INTERNAL_ERROR',
  'internal_error\n', ' internal_error', 'unknown_new_error', CANARIES[0]].entries()) {
  test(`untrusted error class uses a fixed sentinel (${index})`, () => {
    assert.equal(project(withMessages([served({ error_class })])).error_class, 'unknown');
  });
}
test('missing grouping never borrows from raw request or exception text', () => {
  const record = JSON.parse(served()); delete record.fields.route;
  const event = withMessages([JSON.stringify(record)], {
    event: { request: { url: 'https://example.invalid/healthz' }, response: { status: 200 } },
    exceptions: [{ message: 'internal_error GET /healthz' }],
    route: 'GET /healthz', error_class: 'internal_error',
  });
  const result = project(event);
  assert.equal(result.route, 'unknown'); assert.equal(result.error_class, 'unknown');
});
test('conflicting grouping becomes unknown independently of a consistent release', () => {
  const result = project(withMessages([
    served({ error_class: 'internal_error' }),
    served({ route: 'GET /healthz', error_class: 'dependency_unavailable' }),
  ]));
  assert.deepEqual(result, expected({ route: 'unknown' }));
});

// Break caught: unbounded scanning or partial-scan identity laundering. The
// UTF-8 guard above also detects oversized strings passed to JSON.parse.
test('last permitted log entry and message are inspected', () => {
  const logs = Array.from({ length: 63 }, () => ({ message: ['ordinary log'] }));
  logs.push({ message: ['ordinary log', null, false, served()] });
  assert.deepEqual(project(invocation({ logs })), expected());
});
test('strings beyond the log-entry cap are never parsed', () => {
  const hidden = served({ padding: 'beyond-log-cap' });
  const logs = [...Array.from({ length: 64 }, () => ({ message: [served()] })), { message: [hidden] }];
  const start = PARSED_TEXTS.length;
  assertUnknownMetadata(project(invocation({ logs })));
  assert.equal(PARSED_TEXTS.slice(start).includes(hidden), false);
});
test('strings beyond the per-log message cap are never parsed', () => {
  const hidden = served({ padding: 'beyond-message-cap' });
  const start = PARSED_TEXTS.length;
  assertUnknownMetadata(project(withMessages([served(), '', '', '', hidden])));
  assert.equal(PARSED_TEXTS.slice(start).includes(hidden), false);
});
test('a conflict at the final permitted log/message cannot be skipped', () => {
  const logs = [{ message: [served()] }, ...Array.from({ length: 62 }, () => ({ message: [] })),
    { message: ['', '', '', served({ build_sha: SHA_B, release: SHA_B })] }];
  assertUnknownIdentity(project(invocation({ logs })));
});
for (const [name, logs] of [
  ['log cap', [{ message: [served()] }, ...Array.from({ length: 64 }, () => ({ message: [] }))]],
  ['message cap', [{ message: [served(), '', '', '', served({ build_sha: SHA_B, release: SHA_B })] }]],
  ['oversized string', [{ message: [served(), 'x'.repeat(16385)] }]],
  ['malformed logs container', { message: [served()] }],
  ['malformed log entry', [{ message: [served()] }, null]],
  ['malformed message container', [{ message: [served()] }, { message: served() }]],
]) {
  test(`incomplete scan discards metadata but retains platform failure (${name})`, () => {
    const result = project(invocation({ logs, outcome: 'exceededCpu',
      event: { response: { status: 503 } }, exceptions: [{ message: CANARIES[0] }] }));
    assertUnknownMetadata(result);
    assert.equal(result.outcome, 'exceeded_cpu'); assert.equal(result.http_status, 503);
    assert.deepEqual(result.failure_flags, {
      http_5xx: true, exception: true, exceeded_cpu: true, exceeded_memory: false,
    });
  });
}
function sizedServed(bytes, filler = 'x') {
  const record = JSON.parse(served({ padding: '' }));
  const overhead = Buffer.byteLength(JSON.stringify(record), 'utf8');
  const count = Math.floor((bytes - overhead) / Buffer.byteLength(filler, 'utf8'));
  record.fields.padding = filler.repeat(count) + 'x'.repeat(bytes - overhead - count * Buffer.byteLength(filler, 'utf8'));
  const text = JSON.stringify(record);
  assert.equal(Buffer.byteLength(text, 'utf8'), bytes);
  return text;
}
test('a JSON string at exactly the byte cap remains inspectable', () => {
  assert.deepEqual(project(withMessages([sizedServed(16384)])), expected());
});
test('a JSON string one byte above the cap cannot provide identity', () => {
  assertUnknownMetadata(project(withMessages([sizedServed(16385)])));
});
test('the string cap is UTF-8 bytes, not UTF-16 character count', () => {
  const text = sizedServed(16385, 'é');
  assert.ok(text.length < 16384);
  assertUnknownMetadata(project(withMessages([text])));
});
test('oversized scanning after a valid identity still invalidates metadata', () => {
  assertUnknownMetadata(project(withMessages([served(), sizedServed(16385)])));
});
test('malformed JSON and non-string messages do not crash or erase a fully scanned valid record', () => {
  assert.deepEqual(project(withMessages(['{malformed', null, { not: 'a string' }, served()])), expected());
});
for (const [index, logs] of [[], null, 'malformed', [42], [{ message: null }],
  [{ message: ['{', 'null', 'true', '42'] }]].entries()) {
  test(`unusable logging still permits an early resource failure (${index})`, () => {
    const result = project(invocation({ logs, outcome: 'exceededMemory' }));
    assertUnknownMetadata(result);
    assert.equal(result.failure_flags.exceeded_memory, true);
  });
}
test('missing logs still produces a denominator observation with unknown identity', () => {
  const event = invocation(); delete event.logs;
  assert.deepEqual(project(event), expected({ build_sha: 'unknown', release: 'unknown', route: 'unknown' }));
});

// Break caught: pass-through data, object spread, URL rendering, mutation, or
// deriving results from clocks/environment/network instead of explicit inputs.
test('secrets/PII and unknown fields never escape in output, throws or diagnostics', () => {
  const event = withMessages([served({
    authorization: CANARIES[0], email: CANARIES[1], tenant_id: CANARIES[2],
    cookie: CANARIES[3], exception: CANARIES[4],
  }), ...CANARIES.slice(0, 3)], {
    event: { request: { url: `https://example.invalid/${CANARIES[2]}?token=${CANARIES[0]}`,
      method: 'POST', headers: { authorization: CANARIES[0], cookie: CANARIES[3] },
      body: CANARIES[1] }, response: { status: 500, headers: { 'set-cookie': CANARIES[3] }, body: CANARIES[0] } },
    exceptions: [{ name: CANARIES[0], message: CANARIES[1], stack: CANARIES[4] }],
    tenant_id: CANARIES[2], dashboard_url: 'https://example.invalid/private',
    commit_url: 'https://example.invalid/commit/private', discord_payload: { content: CANARIES[0] },
  });
  assert.deepEqual(project(event), expected({ http_status: 500, failure_flags: {
    http_5xx: true, exception: true, exceeded_cpu: false, exceeded_memory: false,
  } }));
  assert.deepEqual(FORBIDDEN, [], 'no diagnostics or external/ambient effects permitted');
});
test('malformed hostile JSON and invalid hostile policy fail safely', () => {
  const result = project(withMessages([`{"secret":"${CANARIES[0]}`, CANARIES[1]]),
    policyFixture({ script_name: CANARIES[0] }));
  assert.equal(result, null);
});
test('deeply frozen inputs/policy are not mutated and repeated calls are deterministic', () => {
  const event = invocation(); const policy = policyFixture();
  const beforeEvent = JSON.stringify(event); const beforePolicy = JSON.stringify(policy);
  deepFreeze(event); deepFreeze(policy);
  assert.deepEqual(project(event, policy), expected());
  assert.deepEqual(project(event, policy), expected());
  assert.equal(JSON.stringify(event), beforeEvent); assert.equal(JSON.stringify(policy), beforePolicy);
});
test('fresh output cannot mutate a later observation, the input, or the policy', () => {
  const event = invocation(); const policy = policyFixture();
  assert.deepEqual(project(event, policy), expected());
  // Exercise the actual returned references; a JSON clone would hide caching.
  // Frozen output is also valid, hence Reflect.set rather than strict assignment.
  Reflect.set(lastRawResult, 'route', 'changed');
  Reflect.set(lastRawResult.failure_flags, 'exception', true);
  assert.deepEqual(project(event, policy), expected());
  assert.equal(event.logs[0].message[0], served());
  assert.deepEqual(policy, policyFixture());
});
