// Independently owned acceptance contract. Builder must not edit this file.
// Import: named synchronous evaluateHealthWindow(samples, policy)
// from ../evaluate-health-window.mjs.
// Command (repo root):
// node --experimental-vm-modules --test workers/observability/tests/evaluate-health-window.test.mjs
//
// The tests exercise the actual ESM in an isolated VM, with imports, ambient
// I/O, logging, clocks, random values, timers, environment access and storage
// forbidden. This is a dependency-free sustained-red window evaluator, not a
// /readyz poller, Tail consumer, scheduler, webhook delivery, dashboard, or
// alerting deployment. Input is an explicit array of health samples the caller
// has already windowed: each sample carries its own ms clock, so window
// membership is the caller's job, not this function's. No cross-window state
// is promised: each call is one window.
//
// Frozen policy (all four required, no coercion or defaults; extras ignored):
// {service: 'api', environment: 'staging',
//  red_for_ms: integer 1..3600000,
//  min_samples: integer 1..100000}
// Only the explicit staging API labels are accepted. Any other service or
// environment value in the policy is invalid. Invalid policy or a non-array
// samples root => null, no throw, never a fabricated verdict.
//
// Sample validity (per array entry):
// - Must be a plain record with t a non-negative integer ms timestamp and ok
//   a boolean (true = healthy, false = red). Non-objects, null, arrays,
//   missing/mistyped t or ok, negative/non-integer/NaN/Infinite t, or
//   non-boolean ok => malformed: skipped, counted, never healthy/red.
// - Valid samples are ordered by t ascending (stable) before evaluation.
//   Window membership is the caller's job; this function never filters by t.
// - Malformed entries break contiguity: no breach may span untrusted
//   evidence. A malformed entry between reds in input order splits the run,
//   even when the surviving valid reds would otherwise span the threshold.
//   Leading/trailing malformed entries do not suppress a clean breach.
//
// Frozen v1 output (no additional fields; key order below is part of the
// contract and JSON output must be byte-identical for the same input):
// {schema_version: 1, service, environment,
//  verdict: 'breach'|'ok'|'unknown',
//  reason: 'breach'|'healthy'|'insufficient_data',
//  counts: {valid, red, malformed},
//  longest_red_span_ms, red_for_ms}
// verdict/reason pairs: breach/breach, ok/healthy, unknown/insufficient_data.
// Breach rule: a contiguous not-ok run in t order (healthy samples and
// malformed evidence both break runs) spans (last_t - first_t) >= red_for_ms.
// A single red sample spans 0 and never breaches. longest_red_span_ms is the
// maximum run span (0 when no red or only singletons), still reported when
// the verdict is unknown. Below floor (valid < min_samples, including empty)
// => unknown, never a breach, even when a run would otherwise span the
// threshold. At or above floor => breach iff any run spans the threshold,
// else ok. red_for_ms echoes the policy threshold verbatim.
// Tests only: no source placeholder; the Builder adds the module, README and CI.

import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import vm from 'node:vm';

const MODULE_URL = new URL('../evaluate-health-window.mjs', import.meta.url);
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
  throw new Error(`evaluator attempted forbidden ambient access: ${name}`);
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
  loadFailure = error.code === 'ENOENT' ? 'missing health-window module' : 'evaluator did not load safely';
}

function evaluate(samples, ...policies) {
  const policy = policies.length === 0 ? policyFixture() : policies[0];
  assert.equal(typeof namespace?.evaluateHealthWindow, 'function', loadFailure ?? 'missing named export');
  const before = FORBIDDEN.length;
  let result;
  let thrown;
  try { result = namespace.evaluateHealthWindow(samples, policy); } catch (error) { thrown = error; }
  if (thrown) assertNoCanaries(String(thrown) + String(thrown?.stack));
  assert.equal(FORBIDDEN.length, before, 'evaluation attempted a forbidden side effect');
  assert.equal(thrown, undefined, 'JSON-compatible input must not throw');
  assert.notEqual(result, undefined, 'evaluation must return a verdict or null');
  assert.equal(typeof result?.then, 'undefined', 'evaluation must be synchronous');
  assertNoCanaries(JSON.stringify(result));
  lastRawResult = result;
  // Normalize only the cross-realm prototypes, not the production field values.
  return result === null ? null : JSON.parse(JSON.stringify(result));
}

function assertNoCanaries(text) {
  for (const canary of CANARIES) {
    assert.equal(text.includes(canary), false, 'evaluator leaked a synthetic secret/PII canary');
  }
}

function policyFixture(overrides = {}) {
  return {
    service: 'api', environment: 'staging', red_for_ms: 120000, min_samples: 2,
    ...overrides,
  };
}

function sample(t, ok, overrides = {}) {
  return { t, ok, ...overrides };
}

function healthy(t, overrides = {}) {
  return sample(t, true, overrides);
}

function red(t, overrides = {}) {
  return sample(t, false, overrides);
}

function expectedOk(overrides = {}) {
  return {
    schema_version: 1, service: 'api', environment: 'staging',
    verdict: 'ok', reason: 'healthy',
    counts: { valid: 2, red: 0, malformed: 0 },
    longest_red_span_ms: 0, red_for_ms: 120000,
    ...overrides,
  };
}

function expectedBreach(overrides = {}) {
  return {
    schema_version: 1, service: 'api', environment: 'staging',
    verdict: 'breach', reason: 'breach',
    counts: { valid: 2, red: 2, malformed: 0 },
    longest_red_span_ms: 120000, red_for_ms: 120000,
    ...overrides,
  };
}

function expectedUnknown(overrides = {}) {
  return {
    schema_version: 1, service: 'api', environment: 'staging',
    verdict: 'unknown', reason: 'insufficient_data',
    counts: { valid: 0, red: 0, malformed: 0 },
    longest_red_span_ms: 0, red_for_ms: 120000,
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
test('loads the named evaluator export without ambient effects or dependencies', () => {
  assert.equal(loadFailure, undefined, 'implement evaluate-health-window.mjs with the frozen named export');
  assert.equal(typeof namespace?.evaluateHealthWindow, 'function');
  assert.deepEqual(FORBIDDEN, []);
});

// Break caught: an unpinned verdict schema, renamed keys, or reordered output.
test('all-healthy window is one exact-schema ok verdict at the floor', () => {
  const got = evaluate([healthy(0), healthy(60000)]);
  const want = expectedOk();
  assert.deepEqual(got, want);
  assert.equal(JSON.stringify(got), JSON.stringify(want));
});

test('output exposes exactly the frozen top-level and nested keys in order', () => {
  const got = evaluate([healthy(0), healthy(60000)]);
  assert.deepEqual(Object.keys(got),
    ['schema_version', 'service', 'environment', 'verdict', 'reason',
      'counts', 'longest_red_span_ms', 'red_for_ms']);
  assert.deepEqual(Object.keys(got.counts), ['valid', 'red', 'malformed']);
});

test('flapping reds without a sustained run stay ok', () => {
  const got = evaluate([red(0), healthy(30000), red(60000), healthy(90000), red(120000)]);
  assert.equal(got.verdict, 'ok');
  assert.equal(got.reason, 'healthy');
  assert.deepEqual(got.counts, { valid: 5, red: 3, malformed: 0 });
  assert.equal(got.longest_red_span_ms, 0);
  assert.equal(got.red_for_ms, 120000);
});

test('one healthy sample inside a red span breaks the run', () => {
  const got = evaluate([red(0), healthy(1), red(120000)]);
  assert.equal(got.verdict, 'ok');
  assert.equal(got.longest_red_span_ms, 0);
});

test('longest span is the maximum run, not the first', () => {
  const got = evaluate([red(0), red(50000), healthy(60000), red(100000), red(200000)]);
  assert.equal(got.verdict, 'ok');
  assert.equal(got.longest_red_span_ms, 100000);
  assert.deepEqual(got.counts, { valid: 5, red: 4, malformed: 0 });
});

test('breach keeps the longest span when several runs exist', () => {
  const got = evaluate([red(0), red(130000), healthy(140000), red(200000), red(210000)]);
  assert.equal(got.verdict, 'breach');
  assert.equal(got.reason, 'breach');
  assert.equal(got.longest_red_span_ms, 130000);
});

// Break caught: an off-by-one at the boundary (breach is >=, never >) or a
// duration derived from sample counts instead of the caller clock.
test('span exactly equal to red_for_ms breaches', () => {
  const got = evaluate([red(0), red(120000)]);
  assert.deepEqual(got, expectedBreach());
});

test('span exactly equal to a small red_for_ms breaches', () => {
  const got = evaluate([red(1000), red(6000)], policyFixture({ red_for_ms: 5000 }));
  assert.equal(got.verdict, 'breach');
  assert.equal(got.reason, 'breach');
  assert.equal(got.longest_red_span_ms, 5000);
  assert.equal(got.red_for_ms, 5000);
});

test('span one millisecond below red_for_ms stays ok', () => {
  const got = evaluate([red(0), red(119999)]);
  assert.equal(got.verdict, 'ok');
  assert.equal(got.reason, 'healthy');
  assert.equal(got.longest_red_span_ms, 119999);
});

test('span one millisecond below a small red_for_ms stays ok', () => {
  const got = evaluate([red(1000), red(5999)], policyFixture({ red_for_ms: 5000 }));
  assert.equal(got.verdict, 'ok');
  assert.equal(got.longest_red_span_ms, 4999);
});

// Break caught: a single red sample treated as a sustained outage.
test('single red sample never breaches at the floor', () => {
  const got = evaluate([red(0)], policyFixture({ red_for_ms: 1, min_samples: 1 }));
  assert.equal(got.verdict, 'ok');
  assert.equal(got.reason, 'healthy');
  assert.deepEqual(got.counts, { valid: 1, red: 1, malformed: 0 });
  assert.equal(got.longest_red_span_ms, 0);
});

test('single healthy sample is ok at the floor', () => {
  const got = evaluate([healthy(42)], policyFixture({ min_samples: 1 }));
  assert.equal(got.verdict, 'ok');
  assert.deepEqual(got.counts, { valid: 1, red: 0, malformed: 0 });
});

// Break caught: an empty or below-floor window reported as ok or as a breach.
test('empty window is unknown, never ok or breach', () => {
  const got = evaluate([]);
  assert.deepEqual(got, expectedUnknown());
});

test('below-floor window suppresses even an obvious breach', () => {
  const got = evaluate([red(0), red(120000)], policyFixture({ min_samples: 3 }));
  assert.equal(got.verdict, 'unknown');
  assert.equal(got.reason, 'insufficient_data');
  assert.deepEqual(got.counts, { valid: 2, red: 2, malformed: 0 });
  assert.equal(got.longest_red_span_ms, 120000);
  assert.equal(got.red_for_ms, 120000);
});

test('single red below the floor is unknown, never a breach', () => {
  const got = evaluate([red(0)]);
  assert.equal(got.verdict, 'unknown');
  assert.equal(got.longest_red_span_ms, 0);
});

test('window exactly at the floor is evaluated', () => {
  const ok = evaluate([healthy(0), healthy(60000)], policyFixture({ min_samples: 2 }));
  assert.equal(ok.verdict, 'ok');
  const breach = evaluate([red(0), red(120000)], policyFixture({ min_samples: 2 }));
  assert.equal(breach.verdict, 'breach');
});

// Break caught: malformed entries counted as healthy/red, echoed back, or
// crashing the evaluator. Expectations are independent literal counts.
for (const [index, entry] of [
  null, undefined, 42, 'sample', true, false, [], [red(0)],
  {}, { t: 0 }, { ok: true }, { t: 0, ok: 1 }, { t: 0, ok: 'false' },
  { t: 0, ok: null }, { t: 0, ok: undefined }, { t: 0, ok: 0 },
  { t: -1, ok: true }, { t: -100, ok: false }, { t: 1.5, ok: true },
  { t: '0', ok: true }, { t: NaN, ok: true }, { t: Infinity, ok: true },
  { t: null, ok: true }, { t: undefined, ok: true }, { t: true, ok: true },
].entries()) {
  test(`malformed entry is skipped and counted, never scored (${index})`, () => {
    const got = evaluate([healthy(0), healthy(60000), entry]);
    assert.deepEqual(got.counts, { valid: 2, red: 0, malformed: 1 });
    assert.equal(got.verdict, 'ok');
    assert.equal(got.longest_red_span_ms, 0);
  });
}

test('malformed entries carrying secrets are never echoed or scored', () => {
  const entry = {
    t: CANARIES[0], ok: CANARIES[1], secret: CANARIES[0], tenant_id: CANARIES[2],
  };
  const got = evaluate([healthy(0), healthy(60000), entry]);
  assert.deepEqual(got.counts, { valid: 2, red: 0, malformed: 1 });
  assert.equal(got.verdict, 'ok');
});

test('throwing getters fail safe as malformed without throwing', () => {
  const hostileT = healthy(120000);
  Object.defineProperty(hostileT, 't', { enumerable: true, get() { throw new Error('hostile t'); } });
  const gotT = evaluate([healthy(0), healthy(60000), hostileT]);
  assert.deepEqual(gotT.counts, { valid: 2, red: 0, malformed: 1 });
  const hostileOk = healthy(120000);
  Object.defineProperty(hostileOk, 'ok', { enumerable: true, get() { throw new Error('hostile ok'); } });
  const gotOk = evaluate([healthy(0), healthy(60000), hostileOk]);
  assert.deepEqual(gotOk.counts, { valid: 2, red: 0, malformed: 1 });
});

// Break caught: untrusted evidence silently filtered so a breach spans a gap
// the evaluator never saw, or any malformed entry suppressing every breach.
test('malformed entry between reds breaks the run', () => {
  const malformed = { t: 60000, ok: 'yes' };
  const got = evaluate([red(0), malformed, red(120000)]);
  assert.equal(got.verdict, 'ok');
  assert.equal(got.reason, 'healthy');
  assert.deepEqual(got.counts, { valid: 2, red: 2, malformed: 1 });
  assert.equal(got.longest_red_span_ms, 0);
});

test('malformed entry with an untrusted clock still breaks the run', () => {
  const got = evaluate([red(0), null, red(120000)]);
  assert.equal(got.verdict, 'ok');
  assert.equal(got.longest_red_span_ms, 0);
});

test('leading malformed entry does not suppress a clean breach', () => {
  const got = evaluate([null, red(0), red(120000)]);
  assert.equal(got.verdict, 'breach');
  assert.equal(got.longest_red_span_ms, 120000);
  assert.deepEqual(got.counts, { valid: 2, red: 2, malformed: 1 });
});

test('trailing malformed entry does not suppress a clean breach', () => {
  const got = evaluate([red(0), red(120000), undefined]);
  assert.equal(got.verdict, 'breach');
  assert.equal(got.longest_red_span_ms, 120000);
});

// Break caught: input order trusted instead of the caller clock, or an
// unstable sort that reorders duplicate timestamps.
test('unsorted healthy between reds stays ok only in t order', () => {
  // Input order shows contiguous reds; t order inserts the healthy sample.
  const got = evaluate([red(0), red(120000), healthy(60000)]);
  assert.equal(got.verdict, 'ok');
  assert.equal(got.longest_red_span_ms, 0);
});

test('unsorted reds breach only in t order', () => {
  // Input order shows a healthy break; t order makes the reds contiguous.
  const got = evaluate([red(0), healthy(200000), red(120000)]);
  assert.equal(got.verdict, 'breach');
  assert.equal(got.longest_red_span_ms, 120000);
});

test('reverse-sorted sustained reds still breach', () => {
  const got = evaluate([red(120000), red(60000), red(0)]);
  assert.equal(got.verdict, 'breach');
  assert.equal(got.longest_red_span_ms, 120000);
});

test('duplicate timestamps span zero and never breach alone', () => {
  const got = evaluate([red(1000), red(1000)], policyFixture({ red_for_ms: 1 }));
  assert.equal(got.verdict, 'ok');
  assert.equal(got.longest_red_span_ms, 0);
});

test('duplicate timestamps with a healthy sample stay broken', () => {
  const got = evaluate([red(1000), healthy(1000), red(1000)]);
  assert.equal(got.verdict, 'ok');
  assert.equal(got.longest_red_span_ms, 0);
});

test('duplicate opening timestamps still span to a later red', () => {
  const got = evaluate([red(0), red(0), red(120000)]);
  assert.equal(got.verdict, 'breach');
  assert.equal(got.longest_red_span_ms, 120000);
});

test('stable order keeps a same-t healthy break after its red', () => {
  // Stable: red(0), healthy(0), red(120000) => broken => ok. An unstable
  // reorder to healthy(0), red(0), red(120000) would falsely breach.
  const got = evaluate([red(0), healthy(0), red(120000)]);
  assert.equal(got.verdict, 'ok');
  assert.equal(got.longest_red_span_ms, 0);
});

test('stable order keeps a same-t red contiguous with a later red', () => {
  // Stable: healthy(0), red(0), red(120000) => contiguous => breach.
  const got = evaluate([healthy(0), red(0), red(120000)]);
  assert.equal(got.verdict, 'breach');
  assert.equal(got.longest_red_span_ms, 120000);
});

// Break caught: an invalid policy producing a fabricated ok/breach verdict.
const invalidPolicies = [
  undefined, null, [], 'staging', 42, true,
  {},
  ...['service', 'environment', 'red_for_ms', 'min_samples']
    .map((key) => { const policy = policyFixture(); delete policy[key]; return policy; }),
  policyFixture({ service: 'web' }), policyFixture({ service: 'API' }),
  policyFixture({ service: 'api-staging' }), policyFixture({ service: '' }),
  policyFixture({ service: 'api ' }), policyFixture({ environment: 'production' }),
  policyFixture({ environment: 'Staging' }), policyFixture({ environment: 'staging-v2' }),
  policyFixture({ environment: '' }), policyFixture({ environment: 'staging ' }),
  policyFixture({ red_for_ms: 0 }), policyFixture({ red_for_ms: 3600001 }),
  policyFixture({ red_for_ms: -1 }), policyFixture({ red_for_ms: 2.5 }),
  policyFixture({ red_for_ms: '120000' }), policyFixture({ red_for_ms: null }),
  policyFixture({ red_for_ms: NaN }), policyFixture({ red_for_ms: Infinity }),
  policyFixture({ min_samples: 0 }), policyFixture({ min_samples: 100001 }),
  policyFixture({ min_samples: -1 }), policyFixture({ min_samples: 2.5 }),
  policyFixture({ min_samples: '2' }), policyFixture({ min_samples: null }),
  policyFixture({ min_samples: NaN }), policyFixture({ min_samples: Infinity }),
  policyFixture({ service: CANARIES[0] }),
];
for (const [index, policy] of invalidPolicies.entries()) {
  test(`invalid policy fails closed to null without a verdict (${index})`, () => {
    assert.equal(evaluate([healthy(0), healthy(60000)], policy), null);
  });
}

test('policy caps at the exact finite limits remain valid', () => {
  const tiny = evaluate([red(0), red(1)],
    policyFixture({ red_for_ms: 1, min_samples: 1 }));
  assert.equal(tiny.verdict, 'breach');
  assert.equal(tiny.longest_red_span_ms, 1);
  const huge = evaluate([red(0), red(3600000)],
    policyFixture({ red_for_ms: 3600000, min_samples: 1 }));
  assert.equal(huge.verdict, 'breach');
  assert.equal(huge.longest_red_span_ms, 3600000);
  const floor = evaluate([healthy(0)], policyFixture({ min_samples: 1 }));
  assert.equal(floor.verdict, 'ok');
  const ceiling = evaluate([healthy(0), healthy(60000)],
    policyFixture({ min_samples: 100000 }));
  assert.equal(ceiling.verdict, 'unknown');
});

test('policy is read on each call, not cached from an earlier window', () => {
  assert.equal(evaluate([healthy(0), healthy(60000)]).verdict, 'ok');
  assert.equal(evaluate([healthy(0), healthy(60000)],
    policyFixture({ environment: 'production' })), null);
  assert.equal(evaluate([healthy(0), healthy(60000)]).verdict, 'ok');
});

test('extra policy fields are ignored without changing the verdict', () => {
  const got = evaluate([healthy(0), healthy(60000)],
    { ...policyFixture(), note: 'extra field', build_sha: 'synthetic' });
  assert.deepEqual(got, expectedOk());
});

test('red_for_ms echoes the policy on every verdict', () => {
  const breach = evaluate([red(0), red(5000)], policyFixture({ red_for_ms: 5000 }));
  assert.equal(breach.red_for_ms, 5000);
  const ok = evaluate([healthy(0), healthy(60000)], policyFixture({ red_for_ms: 5000 }));
  assert.equal(ok.red_for_ms, 5000);
  const unknown = evaluate([], policyFixture({ red_for_ms: 5000 }));
  assert.equal(unknown.red_for_ms, 5000);
});

for (const [index, samples] of [null, undefined, {}, 'window', 42, true].entries()) {
  test(`non-array samples root fails closed to null (${index})`, () => {
    assert.equal(evaluate(samples), null);
  });
}

// Break caught: nondeterministic output, mutated inputs, cached output, or a
// throw/ambient effect on hostile input instead of a clean null/count.
test('same samples and policy produce byte-identical JSON', () => {
  const samples = [red(0), healthy(60000), red(120000), healthy(180000)];
  const policy = policyFixture();
  const first = evaluate(samples, policy);
  const second = evaluate(samples, policy);
  assert.deepEqual(second, first);
  assert.equal(JSON.stringify(second), JSON.stringify(first));
});

test('distinct-t permutations serialize identically after sorting', () => {
  const policy = policyFixture();
  const first = evaluate([red(0), healthy(60000), red(120000)], policy);
  const second = evaluate([red(120000), red(0), healthy(60000)], policy);
  assert.equal(JSON.stringify(second), JSON.stringify(first));
});

test('secrets/PII and unknown fields never escape in output or throws', () => {
  const samples = [
    healthy(0, { authorization: CANARIES[0], email: CANARIES[1] }),
    healthy(60000, { tenant_id: CANARIES[2], cookie: CANARIES[3], stack: CANARIES[4] }),
  ];
  const got = evaluate(samples);
  assert.deepEqual(got, expectedOk());
  assert.deepEqual(FORBIDDEN, [], 'no diagnostics or external/ambient effects permitted');
});

test('deeply frozen inputs/policy are not mutated and repeated calls agree', () => {
  const samples = [healthy(0), healthy(60000)];
  const policy = policyFixture();
  const beforeSamples = JSON.stringify(samples);
  const beforePolicy = JSON.stringify(policy);
  deepFreeze(samples); deepFreeze(policy);
  assert.deepEqual(evaluate(samples, policy), expectedOk());
  assert.deepEqual(evaluate(samples, policy), expectedOk());
  assert.equal(JSON.stringify(samples), beforeSamples);
  assert.equal(JSON.stringify(policy), beforePolicy);
});

test('fresh output cannot mutate a later verdict, the input, or the policy', () => {
  const samples = [healthy(0), healthy(60000)];
  const policy = policyFixture();
  assert.deepEqual(evaluate(samples, policy), expectedOk());
  // Exercise the actual returned references; a JSON clone would hide caching.
  // Frozen output is also valid, hence Reflect.set rather than strict assignment.
  Reflect.set(lastRawResult, 'verdict', 'changed');
  Reflect.set(lastRawResult.counts, 'valid', 999);
  Reflect.set(lastRawResult, 'longest_red_span_ms', 999);
  assert.deepEqual(evaluate(samples, policy), expectedOk());
  assert.equal(samples.length, 2);
  assert.deepEqual(policy, policyFixture());
});

test('hostile throwing proxies fail safe without throwing', () => {
  const proxy = new Proxy(healthy(120000), {
    get() { throw new Error('synthetic boom'); },
  });
  const got = evaluate([healthy(0), healthy(60000), proxy]);
  assert.deepEqual(got.counts, { valid: 2, red: 0, malformed: 1 });
  const policyProxy = new Proxy(policyFixture(), {
    get() { throw new Error('synthetic boom'); },
  });
  assert.equal(evaluate([healthy(0), healthy(60000)], policyProxy), null);
});

test('cyclic extras never throw and never change the verdict', () => {
  const cyclic = healthy(0);
  cyclic.self = cyclic;
  const got = evaluate([cyclic, healthy(60000)]);
  assert.equal(got.verdict, 'ok');
  assert.deepEqual(got.counts, { valid: 2, red: 0, malformed: 0 });
  const policy = policyFixture();
  policy.self = policy;
  const withPolicy = evaluate([healthy(0), healthy(60000)], policy);
  assert.equal(withPolicy.verdict, 'ok');
});

test('no ambient effects across the whole suite', () => {
  assert.deepEqual(FORBIDDEN, []);
});
