// Independently owned acceptance contract. Builder must not edit this file.
// Import: named synchronous evaluateErrorWindow(observations, policy)
// from ../evaluate-error-window.mjs.
// Command (repo root):
// node --experimental-vm-modules --test workers/observability/tests/evaluate-error-window.test.mjs
//
// The tests exercise the actual ESM in an isolated VM, with imports, ambient
// I/O, logging, clocks, random values, timers and environment access forbidden.
// This is a dependency-free window reducer, not a Tail consumer, scheduler,
// webhook delivery, dashboard, or alerting deployment. Input is an explicit
// array of v1 observations the caller has already windowed: v1 observations
// carry no timestamp, so window membership is the caller's job, not this
// function's. No cross-window state is promised: each call is one window.
//
// Frozen policy (all six required, no coercion or defaults; extras ignored):
// {service: 'api', environment: 'staging',
//  min_invocations: integer 1..1000000,
//  max_error_rate: {failed: integer, total: integer} with 0 <= failed <= total,
//    total 1..1000000,
//  per_error_class: {0..64 entries, keys match [a-z][a-z0-9_]{0,63},
//    values are valid rationals},
//  per_release: {0..64 entries, keys are 40 lowercase hex or exactly 'unknown',
//    values are valid rationals}}
// Only the explicit staging API labels are accepted. Any other service or
// environment value in the policy is invalid. Invalid policy or a non-array
// observations root => null, no throw, never a fabricated verdict.
//
// Observation validity (per array entry):
// - Non-objects, null, arrays, schema_version !== 1, non-string service or
//   environment, or failure_flags that is not a plain object with all four of
//   http_5xx/exception/exceeded_cpu/exceeded_memory as booleans => malformed:
//   skipped, counted, never healthy/failed, never echoed back.
// - String service/environment not exactly equal to the policy labels =>
//   excluded: skipped, counted, never merged. No trimming, case folding, or
//   suffix inference in either direction.
// - Otherwise in scope. outcome, http_status, build_sha and unknown extra
//   fields are ignored for counting. Breakdown keys that are invalid fold to
//   an 'unknown' bucket (still counted in totals), they never make the entry
//   malformed: error_class must match [a-z][a-z0-9_]{0,63}; release must be
//   40 lowercase hex or exactly 'unknown'; route must be a canonical
//   uppercase-method + space + safe absolute path (1..128 chars, no controls,
//   exactly one space, path starts with '/', no ? # or backslash, path matches
//   [A-Za-z0-9\-.~!$&'()*+,;=:@%/{}]+) or exactly 'unknown'.
// Failure: any failure_flags entry true => failed (counted once regardless of
// how many flags are set). All four false => healthy. Flags decide; outcome
// and http_status never override them.
//
// Frozen v1 output (no additional fields; key order below is part of the
// contract and JSON output must be byte-identical for the same input order):
// {schema_version: 1, service, environment,
//  verdict: 'pass'|'breach'|'unknown',
//  reason: 'ok'|'breach'|'insufficient_data',
//  counts: {total, failed, healthy, excluded, malformed,
//    by_flag: {http_5xx, exception, exceeded_cpu, exceeded_memory}},
//  rate: {failed, total, decimal},
//  thresholds: {overall: {failed, total, breached}, breached_rules: string[]},
//  breakdowns: {
//    by_error_class: {buckets: [{key, total, failed, decimal}], overflow: {total, failed, distinct_keys}},
//    by_release: {same}, by_route: {same}}}
// verdict/reason pairs: pass/ok, breach/breach, unknown/insufficient_data.
// Below floor (total < min_invocations, including empty) => unknown with an
// empty breached_rules list and overall.breached false, never 0% and never a
// breach. At or above floor => breach iff any rule breaches, else pass.
// Breach rule (exact integer math, strictly greater, never the decimal):
// observed_failed * threshold_total > threshold_failed * observed_total.
// Overall compares the window totals; per-key rules compare that bucket's
// totals when the bucket has total >= 1. Missing keys never breach. The
// 'unknown' bucket is ordinary: per-key rules may name it and it breaches by
// the same comparison.
// breached_rules names: 'overall', 'error_class:<key>', 'release:<key>'.
// Sorted ascending in Unicode code-unit order. No per-route rules.
// decimal: string with exactly 4 fractional digits, half-up
// (floor(x * 10000 + 0.5) / 10000, formatted fixed). total 0 => '0.0000'.
// Breakdowns: distinct keys sorted ascending (code-unit order); the first 32
// become buckets, the rest fold into overflow (overflow.total/failed sum the
// folded buckets; overflow.distinct_keys counts them; zeros when none).
// 'unknown' sorts and caps like any other bucket, never dropped.
// Tests only: no source placeholder; the Builder adds the module, README and CI.

import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import vm from 'node:vm';

const MODULE_URL = new URL('../evaluate-error-window.mjs', import.meta.url);
const SHA_A = '0123456789abcdef0123456789abcdef01234567';
const SHA_B = 'fedcba9876543210fedcba9876543210fedcba98';
const SHA_C = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';
// All canaries are deliberately synthetic, never credentials or real user data.
const CANARIES = [
  'SYNTHETIC_SECRET_NEVER_EMIT_914',
  'synthetic-person-914@example.invalid',
  'synthetic-tenant-914',
  'SYNTHETIC_COOKIE_NEVER_EMIT_914',
  'SYNTHETIC_STACK_NEVER_EMIT_914',
];
const FORBIDDEN = [];
const deny = (name) => (..._args) => {
  FORBIDDEN.push(name);
  throw new Error(`reducer attempted forbidden ambient access: ${name}`);
};
const math = Object.create(Math);
math.random = deny('Math.random');
const context = vm.createContext({
  console: Object.fromEntries(['log', 'info', 'warn', 'error', 'debug', 'trace', 'dir', 'assert']
    .map((name) => [name, deny(`console.${name}`)])),
  fetch: deny('fetch'),
  WebSocket: deny('WebSocket'),
  XMLHttpRequest: deny('XMLHttpRequest'),
  Date: deny('Date'),
  JSON,
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
  loadFailure = error.code === 'ENOENT' ? 'missing reducer module' : 'reducer did not load safely';
}

function evaluate(observations, ...policies) {
  const policy = policies.length === 0 ? policyFixture() : policies[0];
  assert.equal(typeof namespace?.evaluateErrorWindow, 'function', loadFailure ?? 'missing named export');
  const before = FORBIDDEN.length;
  let result;
  let thrown;
  try { result = namespace.evaluateErrorWindow(observations, policy); } catch (error) { thrown = error; }
  if (thrown) assertNoCanaries(String(thrown) + String(thrown.stack));
  assert.equal(FORBIDDEN.length, before, 'reduction attempted a forbidden side effect');
  assert.equal(thrown, undefined, 'JSON-compatible input must not throw');
  assert.notEqual(result, undefined, 'reduction must return a verdict or null');
  assert.equal(typeof result?.then, 'undefined', 'reduction must be synchronous');
  assertNoCanaries(JSON.stringify(result));
  lastRawResult = result;
  // Normalize only the cross-realm prototypes, not the production field values.
  return result === null ? null : JSON.parse(JSON.stringify(result));
}

function assertNoCanaries(text) {
  for (const canary of CANARIES) {
    assert.equal(text.includes(canary), false, 'reducer leaked a synthetic secret/PII canary');
  }
}

function policyFixture(overrides = {}) {
  return {
    service: 'api', environment: 'staging', min_invocations: 4,
    max_error_rate: { failed: 1, total: 4 },
    per_error_class: {}, per_release: {},
    ...overrides,
  };
}

function obs(overrides = {}) {
  return {
    schema_version: 1, service: 'api', environment: 'staging', outcome: 'ok', http_status: 200,
    failure_flags: { http_5xx: false, exception: false, exceeded_cpu: false, exceeded_memory: false },
    build_sha: SHA_A, release: SHA_A, route: 'GET /v1/library/search', error_class: 'unknown',
    ...overrides,
  };
}

function failedObs(overrides = {}) {
  return obs({
    outcome: 'exception', http_status: 500,
    failure_flags: { http_5xx: true, exception: true, exceeded_cpu: false, exceeded_memory: false },
    error_class: 'internal_error',
    ...overrides,
  });
}

function bucket(key, total, failed, decimal) {
  return { key, total, failed, decimal };
}

function emptyOverflow() {
  return { total: 0, failed: 0, distinct_keys: 0 };
}

function expectedVerdict(overrides = {}) {
  return {
    schema_version: 1, service: 'api', environment: 'staging',
    verdict: 'pass', reason: 'ok',
    counts: {
      total: 4, failed: 1, healthy: 3, excluded: 0, malformed: 0,
      by_flag: { http_5xx: 1, exception: 1, exceeded_cpu: 0, exceeded_memory: 0 },
    },
    rate: { failed: 1, total: 4, decimal: '0.2500' },
    thresholds: { overall: { failed: 1, total: 4, breached: false }, breached_rules: [] },
    breakdowns: {
      by_error_class: {
        buckets: [bucket('internal_error', 1, 1, '1.0000'), bucket('unknown', 3, 0, '0.0000')],
        overflow: emptyOverflow(),
      },
      by_release: { buckets: [bucket(SHA_A, 4, 1, '0.2500')], overflow: emptyOverflow() },
      by_route: { buckets: [bucket('GET /v1/library/search', 4, 1, '0.2500')], overflow: emptyOverflow() },
    },
    ...overrides,
  };
}

function deepFreeze(value) {
  if (value && typeof value === 'object') {
    for (const child of Object.values(value)) deepFreeze(child);
    Object.freeze(value);
  }
  return value;
}

// Break caught: a missing export, import-time side effect, or dependency bypass.
test('loads the named reducer export without ambient effects or dependencies', () => {
  assert.equal(loadFailure, undefined, 'implement evaluate-error-window.mjs with the frozen named export');
  assert.equal(typeof namespace?.evaluateErrorWindow, 'function');
  assert.deepEqual(FORBIDDEN, []);
});

// Break caught: losing healthy observations, miscounting a multi-flag failure,
// or emitting an unpinned verdict schema.
test('healthy majority is one exact-schema pass verdict at the floor', () => {
  const observations = [obs(), obs(), obs(), failedObs()];
  const want = expectedVerdict();
  const got = evaluate(observations);
  assert.deepEqual(got, want);
  assert.equal(JSON.stringify(got), JSON.stringify(want));
});

test('output exposes exactly the frozen top-level and nested keys', () => {
  const got = evaluate([obs(), obs(), obs(), failedObs()]);
  assert.deepEqual(Object.keys(got), ['schema_version', 'service', 'environment', 'verdict',
    'reason', 'counts', 'rate', 'thresholds', 'breakdowns']);
  assert.deepEqual(Object.keys(got.counts), ['total', 'failed', 'healthy', 'excluded', 'malformed', 'by_flag']);
  assert.deepEqual(Object.keys(got.counts.by_flag), ['http_5xx', 'exception', 'exceeded_cpu', 'exceeded_memory']);
  assert.deepEqual(Object.keys(got.rate), ['failed', 'total', 'decimal']);
  assert.deepEqual(Object.keys(got.thresholds), ['overall', 'breached_rules']);
  assert.deepEqual(Object.keys(got.thresholds.overall), ['failed', 'total', 'breached']);
  assert.deepEqual(Object.keys(got.breakdowns), ['by_error_class', 'by_release', 'by_route']);
  for (const key of ['by_error_class', 'by_release', 'by_route']) {
    assert.deepEqual(Object.keys(got.breakdowns[key]), ['buckets', 'overflow']);
    assert.deepEqual(Object.keys(got.breakdowns[key].overflow), ['total', 'failed', 'distinct_keys']);
    for (const entry of got.breakdowns[key].buckets) {
      assert.deepEqual(Object.keys(entry), ['key', 'total', 'failed', 'decimal']);
    }
  }
});

// Break caught: threshold evaluated on the rounded decimal instead of the exact
// rational, or an off-by-one at the boundary (breach is strictly greater).
test('overall breach names the rule and keeps exact rationals', () => {
  const observations = [obs(), failedObs(), failedObs(), failedObs()];
  const got = evaluate(observations);
  assert.equal(got.verdict, 'breach');
  assert.equal(got.reason, 'breach');
  assert.deepEqual(got.rate, { failed: 3, total: 4, decimal: '0.7500' });
  assert.deepEqual(got.thresholds.overall, { failed: 1, total: 4, breached: true });
  assert.deepEqual(got.thresholds.breached_rules, ['overall']);
});

test('observed rate exactly at the threshold is a pass, not a breach', () => {
  const observations = [obs(), obs(), obs(), failedObs()];
  const got = evaluate(observations, policyFixture({ max_error_rate: { failed: 1, total: 4 } }));
  assert.equal(got.verdict, 'pass');
  assert.deepEqual(got.thresholds.breached_rules, []);
});

test('breach uses exact integer math even when decimals round equally', () => {
  // 1/3 rounds to 0.3333 and so does 3333/10000, but 1*10000 > 3333*3.
  const observations = [obs(), obs(), failedObs()];
  const got = evaluate(observations, policyFixture({
    min_invocations: 3, max_error_rate: { failed: 3333, total: 10000 },
  }));
  assert.equal(got.rate.decimal, '0.3333');
  assert.equal(got.thresholds.overall.breached, true);
  assert.deepEqual(got.thresholds.breached_rules, ['overall']);
  assert.equal(got.verdict, 'breach');
});

test('zero threshold breaches on any failure; full threshold never breaches', () => {
  const oneFailed = [obs(), obs(), obs(), failedObs()];
  const zero = evaluate(oneFailed, policyFixture({ max_error_rate: { failed: 0, total: 1 } }));
  assert.equal(zero.thresholds.overall.breached, true);
  const full = evaluate(oneFailed, policyFixture({ max_error_rate: { failed: 1, total: 1 } }));
  assert.equal(full.thresholds.overall.breached, false);
  const allFailed = [failedObs(), failedObs(), failedObs(), failedObs()];
  const fullBreach = evaluate(allFailed, policyFixture({ max_error_rate: { failed: 1, total: 1 } }));
  assert.equal(fullBreach.thresholds.overall.breached, false);
  assert.equal(fullBreach.verdict, 'pass');
});

// Break caught: an empty or below-floor window reported as 0% or as a breach.
test('empty window is unknown with a zero rational, never 0% pass or breach', () => {
  const got = evaluate([]);
  assert.equal(got.verdict, 'unknown');
  assert.equal(got.reason, 'insufficient_data');
  assert.deepEqual(got.rate, { failed: 0, total: 0, decimal: '0.0000' });
  assert.deepEqual(got.thresholds.breached_rules, []);
  assert.equal(got.thresholds.overall.breached, false);
  assert.deepEqual(got.counts, {
    total: 0, failed: 0, healthy: 0, excluded: 0, malformed: 0,
    by_flag: { http_5xx: 0, exception: 0, exceeded_cpu: 0, exceeded_memory: 0 },
  });
});

test('below-floor window suppresses even an obvious breach', () => {
  const got = evaluate([failedObs(), failedObs(), failedObs()],
    policyFixture({ min_invocations: 4, max_error_rate: { failed: 0, total: 1 } }));
  assert.equal(got.verdict, 'unknown');
  assert.equal(got.reason, 'insufficient_data');
  assert.deepEqual(got.thresholds.breached_rules, []);
  assert.equal(got.thresholds.overall.breached, false);
  assert.deepEqual(got.rate, { failed: 3, total: 3, decimal: '1.0000' });
});

test('window exactly at the floor is evaluated', () => {
  const got = evaluate([obs(), obs(), obs(), failedObs()], policyFixture({ min_invocations: 4 }));
  assert.equal(got.verdict, 'pass');
  const breach = evaluate([failedObs(), failedObs(), failedObs(), failedObs()],
    policyFixture({ min_invocations: 4 }));
  assert.equal(breach.verdict, 'breach');
});

// Break caught: suffix guessing, production merging, or trusting observation
// labels instead of the explicit policy pair.
for (const [index, labels] of [
  { service: 'api-staging', environment: 'staging' },
  { service: 'api', environment: 'staging-v2' },
  { service: 'api', environment: 'production' },
  { service: 'API', environment: 'staging' },
  { service: 'api', environment: 'STAGING' },
  { service: 'api ', environment: 'staging' },
  { service: 'api', environment: 'staging ' },
  { service: 'ingest', environment: 'staging' },
  { service: 'api', environment: '' },
].entries()) {
  test(`nonmatching labels are excluded and counted, never merged (${index})`, () => {
    const observations = [obs(), obs(), obs(), obs({ ...labels })];
    const got = evaluate(observations);
    assert.equal(got.counts.excluded, 1);
    assert.equal(got.counts.total, 3);
    assert.equal(got.verdict, 'unknown');
    assert.deepEqual(got.thresholds.breached_rules, []);
  });
}

test('excluded observations never contribute to flags, rates or breakdowns', () => {
  const observations = [obs(), obs(), obs(), failedObs({ service: 'ingest' })];
  const got = evaluate(observations, policyFixture({ min_invocations: 3 }));
  assert.deepEqual(got.counts.by_flag, { http_5xx: 0, exception: 0, exceeded_cpu: 0, exceeded_memory: 0 });
  assert.deepEqual(got.rate, { failed: 0, total: 3, decimal: '0.0000' });
  assert.equal(got.breakdowns.by_error_class.buckets.some((entry) => entry.key === 'internal_error'), false);
});

// Break caught: counting a multi-flag invocation more than once, or deriving
// failure from outcome/status instead of the projector's failure flags.
test('one invocation with every flag set counts once with separate flag totals', () => {
  const observations = [obs(), obs(), obs(), obs({
    failure_flags: { http_5xx: true, exception: true, exceeded_cpu: true, exceeded_memory: true },
  })];
  const got = evaluate(observations);
  assert.equal(got.counts.failed, 1);
  assert.equal(got.counts.healthy, 3);
  assert.deepEqual(got.counts.by_flag, { http_5xx: 1, exception: 1, exceeded_cpu: 1, exceeded_memory: 1 });
});

test('each flag is counted independently across the window', () => {
  const observations = [
    obs({ failure_flags: { http_5xx: true, exception: false, exceeded_cpu: false, exceeded_memory: false } }),
    obs({ failure_flags: { http_5xx: false, exception: true, exceeded_cpu: false, exceeded_memory: false } }),
    obs({ failure_flags: { http_5xx: false, exception: false, exceeded_cpu: true, exceeded_memory: false } }),
    obs({ failure_flags: { http_5xx: false, exception: false, exceeded_cpu: false, exceeded_memory: true } }),
  ];
  const got = evaluate(observations);
  assert.equal(got.counts.failed, 4);
  assert.deepEqual(got.counts.by_flag, { http_5xx: 1, exception: 1, exceeded_cpu: 1, exceeded_memory: 1 });
});

test('flags decide failure even when outcome and status disagree', () => {
  const observations = [
    obs({ outcome: 'exception', http_status: 500 }),
    obs({ outcome: 'ok', http_status: 200 }),
    obs({ outcome: 'ok', http_status: 200 }),
    obs({ outcome: 'ok', http_status: 200, failure_flags: {
      http_5xx: false, exception: false, exceeded_cpu: false, exceeded_memory: true,
    } }),
  ];
  const got = evaluate(observations);
  assert.equal(got.counts.failed, 1);
  assert.deepEqual(got.counts.by_flag, { http_5xx: 0, exception: 0, exceeded_cpu: 0, exceeded_memory: 1 });
});

// Break caught: malformed entries counted as healthy/failed, echoed back, or
// crashing the reducer. Expectations are independent literal counts.
for (const [index, entry] of [
  null, undefined, 42, 'observation', true, [],
  { schema_version: 2, service: 'api', environment: 'staging',
    failure_flags: { http_5xx: false, exception: false, exceeded_cpu: false, exceeded_memory: false } },
  { schema_version: '1', service: 'api', environment: 'staging',
    failure_flags: { http_5xx: false, exception: false, exceeded_cpu: false, exceeded_memory: false } },
  { service: 'api', environment: 'staging',
    failure_flags: { http_5xx: false, exception: false, exceeded_cpu: false, exceeded_memory: false } },
  obs({ service: 42 }), obs({ environment: null }), obs({ failure_flags: null }),
  obs({ failure_flags: { http_5xx: false, exception: false, exceeded_cpu: false } }),
  obs({ failure_flags: { http_5xx: 0, exception: false, exceeded_cpu: false, exceeded_memory: false } }),
  obs({ failure_flags: [] }),
].entries()) {
  test(`malformed entry is skipped and counted, never scored (${index})`, () => {
    const got = evaluate([obs(), obs(), obs(), entry]);
    assert.equal(got.counts.malformed, 1);
    assert.equal(got.counts.total, 3);
    assert.equal(got.counts.failed, 0);
    assert.equal(got.verdict, 'unknown');
  });
}

test('malformed entries carrying secrets are never echoed or scored', () => {
  const observations = [obs(), obs(), obs(), {
    schema_version: 1, service: CANARIES[0], environment: CANARIES[2],
    failure_flags: { http_5xx: true, exception: true, exceeded_cpu: true, exceeded_memory: true },
    release: CANARIES[0], route: CANARIES[1], error_class: CANARIES[0], secret: CANARIES[3],
  }];
  const got = evaluate(observations, policyFixture({ min_invocations: 3 }));
  assert.equal(got.counts.excluded, 1);
  assert.equal(got.counts.failed, 0);
  assert.deepEqual(got.counts.by_flag, { http_5xx: 0, exception: 0, exceeded_cpu: 0, exceeded_memory: 0 });
});

test('throwing getters fail safe as malformed without throwing', () => {
  const hostile = obs();
  Object.defineProperty(hostile, 'failure_flags', { enumerable: true, get() { throw new Error('hostile getter'); } });
  const got = evaluate([obs(), obs(), obs(), hostile]);
  assert.equal(got.counts.malformed, 1);
  assert.equal(got.counts.total, 3);
});

// Break caught: an invalid policy producing a fabricated pass/breach verdict.
const invalidPolicies = [
  undefined, null, [], 'staging', {},
  ...['service', 'environment', 'min_invocations', 'max_error_rate', 'per_error_class', 'per_release']
    .map((key) => { const policy = policyFixture(); delete policy[key]; return policy; }),
  policyFixture({ service: 'observer' }), policyFixture({ environment: 'production' }),
  policyFixture({ service: 'api-staging' }), policyFixture({ environment: 'staging-v2' }),
  policyFixture({ service: 'API' }), policyFixture({ environment: 'STAGING' }),
  policyFixture({ service: 'api ' }), policyFixture({ environment: 'staging\n' }),
  policyFixture({ min_invocations: 0 }), policyFixture({ min_invocations: 1000001 }),
  policyFixture({ min_invocations: 2.5 }), policyFixture({ min_invocations: '4' }),
  policyFixture({ min_invocations: null }),
  policyFixture({ max_error_rate: null }), policyFixture({ max_error_rate: [] }),
  policyFixture({ max_error_rate: { failed: 1 } }), policyFixture({ max_error_rate: { total: 4 } }),
  policyFixture({ max_error_rate: { failed: -1, total: 4 } }),
  policyFixture({ max_error_rate: { failed: 2, total: 1 } }),
  policyFixture({ max_error_rate: { failed: 1, total: 0 } }),
  policyFixture({ max_error_rate: { failed: 1.5, total: 4 } }),
  policyFixture({ max_error_rate: { failed: '1', total: 4 } }),
  policyFixture({ per_error_class: [] }), policyFixture({ per_release: [] }),
  policyFixture({ per_error_class: { Bad_Name: { failed: 1, total: 4 } } }),
  policyFixture({ per_error_class: { ['a'.repeat(65)]: { failed: 1, total: 4 } } }),
  policyFixture({ per_error_class: { internal_error: { failed: 2, total: 1 } } }),
  policyFixture({ per_release: { NOTHEX: { failed: 1, total: 4 } } }),
  policyFixture({ per_release: { [SHA_A.slice(0, 39)]: { failed: 1, total: 4 } } }),
  policyFixture({ per_release: { [SHA_A]: { failed: 1, total: 0 } } }),
  policyFixture({ per_error_class: Object.fromEntries(
    Array.from({ length: 65 }, (_, i) => [`error_${i}`, { failed: 1, total: 4 }])) }),
  policyFixture({ per_release: Object.fromEntries(
    Array.from({ length: 65 }, (_, i) => [(i + 1).toString(16).padStart(40, '0'), { failed: 1, total: 4 }])) }),
  policyFixture({ service: CANARIES[0] }),
];
for (const [index, policy] of invalidPolicies.entries()) {
  test(`invalid policy fails closed to null without a verdict (${index})`, () => {
    assert.equal(evaluate([obs(), obs(), obs(), failedObs()], policy), null);
  });
}

test('policy caps at the exact finite limits remain valid', () => {
  const policy = policyFixture({
    min_invocations: 1,
    per_error_class: Object.fromEntries(
      Array.from({ length: 64 }, (_, i) => [`error_${i}`, { failed: 1, total: 1 }])),
    per_release: Object.fromEntries(
      Array.from({ length: 64 }, (_, i) => [(i + 1).toString(16).padStart(40, '0'), { failed: 1, total: 1 }])),
  });
  const got = evaluate([obs()], policy);
  assert.equal(got.verdict, 'pass');
});

test('policy is read on each call, not cached from an earlier window', () => {
  assert.equal(evaluate([obs(), obs(), obs(), failedObs()]).verdict, 'pass');
  assert.equal(evaluate([obs(), obs(), obs(), failedObs()],
    policyFixture({ environment: 'production' })), null);
});

test('extra policy fields are ignored without changing the verdict', () => {
  const got = evaluate([obs(), obs(), obs(), failedObs()],
    { ...policyFixture(), build_sha: SHA_B, release: SHA_B, script_name: 'other-producer' });
  assert.deepEqual(got, expectedVerdict());
});

for (const [index, observations] of [null, undefined, {}, 'window', 42, true].entries()) {
  test(`non-array observations root fails closed to null (${index})`, () => {
    assert.equal(evaluate(observations), null);
  });
}

// Break caught: per-key caps ignored, missing keys breaching, unknown dropped,
// or breached rules reported without names/sorting.
test('per-error-class breach names the key alongside the overall rule', () => {
  const observations = [obs(), obs(), obs(), failedObs()];
  const got = evaluate(observations, policyFixture({
    per_error_class: { internal_error: { failed: 0, total: 1 } },
  }));
  assert.equal(got.verdict, 'breach');
  assert.deepEqual(got.thresholds.breached_rules, ['error_class:internal_error']);
  assert.equal(got.thresholds.overall.breached, false);
});

test('per-release breach uses that release bucket totals only', () => {
  const observations = [
    obs({ release: SHA_A }), obs({ release: SHA_A }),
    failedObs({ release: SHA_B }), failedObs({ release: SHA_B }),
  ];
  const got = evaluate(observations, policyFixture({
    max_error_rate: { failed: 1, total: 1 },
    per_release: { [SHA_B]: { failed: 1, total: 4 } },
  }));
  assert.equal(got.verdict, 'breach');
  assert.deepEqual(got.thresholds.breached_rules, [`release:${SHA_B}`]);
});

test('per-key rules for absent keys never breach', () => {
  const observations = [obs(), obs(), obs(), obs()];
  const got = evaluate(observations, policyFixture({
    per_error_class: { internal_error: { failed: 0, total: 1 } },
    per_release: { [SHA_B]: { failed: 0, total: 1 } },
  }));
  assert.equal(got.verdict, 'pass');
  assert.deepEqual(got.thresholds.breached_rules, []);
});

test('below-floor window suppresses per-key breaches too', () => {
  const got = evaluate([failedObs()], policyFixture({
    min_invocations: 4,
    per_error_class: { internal_error: { failed: 0, total: 1 } },
    per_release: { [SHA_A]: { failed: 0, total: 1 } },
  }));
  assert.equal(got.verdict, 'unknown');
  assert.deepEqual(got.thresholds.breached_rules, []);
});

test('multiple breached rules are all named in sorted order', () => {
  const observations = [failedObs({ release: SHA_B }), failedObs({ release: SHA_B }),
    failedObs({ release: SHA_B }), failedObs({ release: SHA_B })];
  const got = evaluate(observations, policyFixture({
    max_error_rate: { failed: 0, total: 1 },
    per_error_class: { internal_error: { failed: 0, total: 1 } },
    per_release: { [SHA_B]: { failed: 0, total: 1 } },
  }));
  assert.deepEqual(got.thresholds.breached_rules,
    ['error_class:internal_error', 'overall', `release:${SHA_B}`]);
});

test('unknown release still counts and breaches under its own per-release rule', () => {
  const observations = [obs({ release: 'unknown' }), obs({ release: 'unknown' }),
    failedObs({ release: 'unknown' }), failedObs({ release: 'unknown' })];
  const got = evaluate(observations, policyFixture({
    max_error_rate: { failed: 1, total: 1 },
    per_release: { unknown: { failed: 1, total: 4 } },
  }));
  assert.equal(got.counts.total, 4);
  assert.deepEqual(got.breakdowns.by_release.buckets, [bucket('unknown', 4, 2, '0.5000')]);
  assert.deepEqual(got.thresholds.breached_rules, ['release:unknown']);
});

// Break caught: invalid breakdown keys echoed as distinct buckets, unknown
// dropped, or adversarial cardinality producing unbounded output.
test('invalid breakdown keys fold into unknown without losing totals', () => {
  const observations = [
    obs({ release: 'NOTHEX', error_class: 'Bad!', route: 'not a route' }),
    obs({ release: 42, error_class: null, route: 'GET /search?q=synthetic' }),
    obs(),
    failedObs(),
  ];
  const got = evaluate(observations);
  assert.equal(got.counts.total, 4);
  assert.equal(got.counts.malformed, 0);
  assert.deepEqual(got.breakdowns.by_release.buckets,
    [bucket(SHA_A, 2, 1, '0.5000'), bucket('unknown', 2, 0, '0.0000')]);
  assert.deepEqual(got.breakdowns.by_error_class.buckets,
    [bucket('internal_error', 1, 1, '1.0000'), bucket('unknown', 3, 0, '0.0000')]);
  assert.deepEqual(got.breakdowns.by_route.buckets,
    [bucket('GET /v1/library/search', 2, 1, '0.5000'), bucket('unknown', 2, 0, '0.0000')]);
});

test('breakdown buckets sort ascending and carry exact rationals plus decimals', () => {
  const observations = [
    failedObs({ release: SHA_B, error_class: 'dependency_unavailable', route: 'GET /healthz' }),
    obs({ release: SHA_A, error_class: 'internal_error', route: 'DELETE /v1/credentials/{id}' }),
    obs({ release: SHA_A, error_class: 'internal_error', route: 'DELETE /v1/credentials/{id}' }),
    obs({ release: SHA_A, error_class: 'internal_error', route: 'DELETE /v1/credentials/{id}' }),
  ];
  const got = evaluate(observations);
  assert.deepEqual(got.breakdowns.by_error_class.buckets, [
    bucket('dependency_unavailable', 1, 1, '1.0000'),
    bucket('internal_error', 3, 0, '0.0000'),
  ]);
  assert.deepEqual(got.breakdowns.by_release.buckets, [
    bucket(SHA_A, 3, 0, '0.0000'),
    bucket(SHA_B, 1, 1, '1.0000'),
  ]);
  assert.deepEqual(got.breakdowns.by_route.buckets, [
    bucket('DELETE /v1/credentials/{id}', 3, 0, '0.0000'),
    bucket('GET /healthz', 1, 1, '1.0000'),
  ]);
});

function distinctReleases(count) {
  return Array.from({ length: count }, (_, i) => {
    const release = (i + 1).toString(16).padStart(40, '0');
    return i % 2 === 0 ? obs({ release }) : failedObs({ release });
  });
}

test('release breakdown caps at 32 buckets with a summed overflow bucket', () => {
  const observations = distinctReleases(35);
  const got = evaluate(observations, policyFixture({ min_invocations: 1 }));
  const buckets = got.breakdowns.by_release.buckets;
  assert.equal(buckets.length, 32);
  assert.deepEqual(buckets, [...buckets].sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : 0)));
  assert.equal(buckets[0].key, '0000000000000000000000000000000000000001');
  const foldedKeys = [...new Set(observations.map((entry) => entry.release))].sort().slice(32);
  assert.equal(got.breakdowns.by_release.overflow.distinct_keys, 3);
  const folded = observations.filter((entry) => foldedKeys.includes(entry.release));
  assert.equal(got.breakdowns.by_release.overflow.total, folded.length);
  assert.equal(got.breakdowns.by_release.overflow.failed,
    folded.filter((entry) => entry.failure_flags.exception).length);
  const bucketedTotal = buckets.reduce((sum, entry) => sum + entry.total, 0);
  assert.equal(bucketedTotal + got.breakdowns.by_release.overflow.total, 35);
});

test('error-class and route breakdowns each cap with their own overflow', () => {
  const errorClasses = Array.from({ length: 33 }, (_, i) => obs({ error_class: `error_${i}` }));
  const errorGot = evaluate(errorClasses, policyFixture({ min_invocations: 1 }));
  assert.equal(errorGot.breakdowns.by_error_class.buckets.length, 32);
  assert.equal(errorGot.breakdowns.by_error_class.overflow.distinct_keys, 1);
  const routes = Array.from({ length: 33 }, (_, i) => obs({ route: `GET /route${i}` }));
  const routeGot = evaluate(routes, policyFixture({ min_invocations: 1 }));
  assert.equal(routeGot.breakdowns.by_route.buckets.length, 32);
  assert.equal(routeGot.breakdowns.by_route.overflow.distinct_keys, 1);
});

test('unknown bucket competes for the cap like any other bucket', () => {
  const observations = [obs({ release: 'unknown' }), ...distinctReleases(32)];
  const got = evaluate(observations, policyFixture({ min_invocations: 1 }));
  assert.equal(got.breakdowns.by_release.buckets.length, 32);
  assert.equal(got.breakdowns.by_release.overflow.distinct_keys, 1);
  const keys = got.breakdowns.by_release.buckets.map((entry) => entry.key);
  // 'unknown' sorts after all hex releases, so it is the folded key here; its
  // counts must survive in overflow rather than disappear.
  assert.equal(keys.includes('unknown'), false);
  assert.equal(got.breakdowns.by_release.overflow.total, 1);
});

// Break caught: a decimal-only contract, an unfrozen rounding rule, or
// nondeterministic key order across identical calls.
for (const [failed, total, decimal] of [
  [0, 5, '0.0000'], [1, 4, '0.2500'], [1, 3, '0.3333'], [2, 3, '0.6667'],
  [1, 6, '0.1667'], [1, 20000, '0.0001'], [1, 40000, '0.0000'],
  [9999, 10000, '0.9999'], [19999, 20000, '1.0000'], [1, 1, '1.0000'],
]) {
  test(`human decimal rounds half-up to four places (${failed}/${total})`, () => {
    const observations = [
      ...Array.from({ length: total - failed }, () => obs()),
      ...Array.from({ length: failed }, () => failedObs()),
    ];
    const got = evaluate(observations, policyFixture({ min_invocations: 1 }));
    assert.deepEqual(got.rate, { failed, total, decimal });
  });
}

test('same input order and policy produce byte-identical JSON', () => {
  const observations = [failedObs({ release: SHA_B }), obs(), obs({ release: SHA_C }), obs()];
  const policy = policyFixture({ per_release: { [SHA_B]: { failed: 1, total: 4 } } });
  const first = evaluate(observations, policy);
  const second = evaluate(observations, policy);
  assert.deepEqual(second, first);
  assert.equal(JSON.stringify(second), JSON.stringify(first));
});

test('same multiset in a different order still serializes identically', () => {
  const policy = policyFixture({ min_invocations: 1 });
  const first = evaluate([failedObs({ release: SHA_B }), obs(), obs()], policy);
  const second = evaluate([obs(), obs(), failedObs({ release: SHA_B })], policy);
  assert.equal(JSON.stringify(second), JSON.stringify(first));
});

// Break caught: pass-through data, object spread, mutation, or deriving results
// from clocks/environment/network instead of explicit inputs.
test('secrets/PII and unknown fields never escape in output or throws', () => {
  const observations = [
    obs({ authorization: CANARIES[0], email: CANARIES[1], tenant_id: CANARIES[2] }),
    obs({ cookie: CANARIES[3], stack: CANARIES[4], dashboard_url: 'https://example.invalid/private' }),
    obs(),
    failedObs({ exception_message: CANARIES[4], trace_id: CANARIES[2] }),
  ];
  const got = evaluate(observations);
  assert.deepEqual(got, expectedVerdict());
  assert.deepEqual(FORBIDDEN, [], 'no diagnostics or external/ambient effects permitted');
});

test('deeply frozen inputs/policy are not mutated and repeated calls are deterministic', () => {
  const observations = [obs(), obs(), obs(), failedObs()];
  const policy = policyFixture();
  const beforeObservations = JSON.stringify(observations);
  const beforePolicy = JSON.stringify(policy);
  deepFreeze(observations); deepFreeze(policy);
  assert.deepEqual(evaluate(observations, policy), expectedVerdict());
  assert.deepEqual(evaluate(observations, policy), expectedVerdict());
  assert.equal(JSON.stringify(observations), beforeObservations);
  assert.equal(JSON.stringify(policy), beforePolicy);
});

test('fresh output cannot mutate a later verdict, the input, or the policy', () => {
  const observations = [obs(), obs(), obs(), failedObs()];
  const policy = policyFixture();
  assert.deepEqual(evaluate(observations, policy), expectedVerdict());
  // Exercise the actual returned references; a JSON clone would hide caching.
  // Frozen output is also valid, hence Reflect.set rather than strict assignment.
  Reflect.set(lastRawResult, 'verdict', 'changed');
  Reflect.set(lastRawResult.counts, 'failed', 999);
  Reflect.set(lastRawResult.counts.by_flag, 'exception', true);
  Reflect.set(lastRawResult.rate, 'decimal', 'changed');
  assert.deepEqual(evaluate(observations, policy), expectedVerdict());
  assert.equal(observations.length, 4);
  assert.deepEqual(policy, policyFixture());
});
