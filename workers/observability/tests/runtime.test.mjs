// Frozen, independently authored runtime acceptance. Builder: do not edit this
// file or runtime-harness.mjs. Implement runtime/config/docs/CI, not these tests.
// node --experimental-vm-modules --test workers/observability/tests/runtime.test.mjs
//
// Breaks caught: dropping healthy denominators; wrong producer/labels; prefix
// sampling on overflow; event-time or closed-window mixing; lost durable state;
// double alarm/delivery; holding a transaction across I/O; secret persistence;
// missing pre-I/O cooldown; unbounded response/deadline; public ingestion; live
// example configuration. No real credential, endpoint, Cloudflare or deploy is
// used. All transport is a VM fake; existing pure modules execute unchanged.
//
// This slice intentionally stops at RED: no runtime placeholder is supplied.
// Delivery is at-most-once ATTEMPT, not exactly-once delivery: a crash after the
// durable reservation but before POST can lose an alert. Retry/outbox semantics
// need a future separately specified change, never an implicit retry here.

import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { test } from 'node:test';
import { projectTailEvent } from '../project-tail-event.mjs';
import { evaluateErrorWindow } from '../evaluate-error-window.mjs';
import { renderVerdictAlert } from '../render-verdict-alert.mjs';
import { Clock, Storage, START, deferred, harness, response, settle } from './runtime-harness.mjs';

const SCRIPT = 'nntune-api-staging';
const SHA = '0123456789abcdef0123456789abcdef01234567';
const OTHER_SHA = 'fedcba9876543210fedcba9876543210fedcba98';
const LOGS = 'https://logs.example.invalid/view/17';
const POLICY = {
  service: 'api', environment: 'staging', min_invocations: 20,
  max_error_rate: { failed: 1, total: 20 }, per_error_class: {}, per_release: {},
};
const PROJECTION_POLICY = {
  script_name: SCRIPT, service: 'api', environment: 'staging',
  routes: ['GET /health'], error_classes: ['upstream'],
};
const CANARIES = [
  'SYNTHETIC_PRIVATE_2837', 'synthetic-person-2837@example.invalid',
  'SYNTHETIC_COOKIE_2837', 'SYNTHETIC_STACK_2837',
];

function event({ status = 200, outcome = 'ok', scriptName = SCRIPT, sha = SHA } = {}) {
  return {
    scriptName, outcome, eventTimestamp: 1,
    event: {
      request: { url: `https://example.invalid/${CANARIES[0]}`, headers: { cookie: CANARIES[2] } },
      response: { status },
    },
    exceptions: outcome === 'exception' ? [{ name: 'Error', message: CANARIES[3], stack: CANARIES[3] }] : [],
    logs: [{ level: 'log', timestamp: 1, message: [JSON.stringify({
      ts: '2001-01-01T00:00:00Z', level: 'info', worker: 'api',
      trace_id: 'a'.repeat(32), event: 'api.request.served',
      fields: { build_sha: sha, release: sha, route: 'GET /health', error_class: 'upstream', private: CANARIES[1],
        service: 'untrusted-other', environment: 'production' },
    })] }],
  };
}
function observation(failed = false, extras = {}) {
  return {
    schema_version: 1, service: 'api', environment: 'staging', outcome: 'ok',
    http_status: failed ? 503 : 200,
    failure_flags: { http_5xx: failed, exception: false, exceeded_cpu: false, exceeded_memory: false },
    build_sha: SHA, release: SHA, route: 'GET /health', error_class: 'upstream', ...extras,
  };
}
const batch = (failed, total = 20, extras = {}) => Array.from({ length: total }, (_, i) => observation(i < failed, extras));
const input = (observations, incomplete = false) => ({ observations, incomplete });
const serialized = (value) => JSON.stringify(value);
function noCanaries(value) {
  const text = typeof value === 'string' ? value : serialized(value);
  for (const canary of CANARIES) assert.equal(text?.includes(canary) ?? false, false, 'synthetic secret/PII escaped');
}
function strings(value, result = []) {
  if (typeof value === 'string') result.push(value);
  else if (value && typeof value === 'object') for (const child of Object.values(value)) strings(child, result);
  return result;
}
function recordCount(value) {
  if (!value || typeof value !== 'object') return 0;
  if (value.schema_version === 1 && value.failure_flags) return 1;
  return Object.values(value).reduce((sum, child) => sum + recordCount(child), 0);
}
function bounded(h) {
  for (const snapshot of h.storage.history) {
    let count = 0;
    let totalBytes = 0;
    for (const [key, value] of snapshot.values) {
      assert.ok(Buffer.byteLength(key) <= 2048, 'unbounded storage key');
      const size = Buffer.byteLength(serialized(value));
      assert.ok(size <= 65536, 'per-value 64KiB bound');
      totalBytes += size;
      count += recordCount(value);
    }
    assert.ok(count <= 2048, 'retained more than 2048 observations');
    // Loose representation-independent envelope: even maximal v1 records plus
    // bounded window/cooldown metadata fit well below this. Compaction is legal.
    assert.ok(totalBytes <= 2 * 1024 * 1024, 'unbounded total durable state');
    noCanaries(snapshot);
  }
}
function incompleteEffects(h, replies = []) {
  function visit(value) {
    if (!value || typeof value !== 'object') return;
    assert.notEqual(value.verdict, 'pass', 'lost coverage must not be exposed as a healthy/pass verdict');
    for (const child of Object.values(value)) visit(child);
  }
  visit([h.storage.history, replies]);
  privateEffects(h, replies);
}
function privateEffects(h, replies = []) {
  noCanaries(h.calls.rpc);
  noCanaries(h.storage.history);
  noCanaries(replies);
  noCanaries(h.calls.fetch.map(({ body }) => body));
  assert.deepEqual(h.calls.console, [], 'runtime must not console-log telemetry or delivery errors');
  const secrets = [h.env.OPS_ALERT_WEBHOOK_URL, 'synthetic-local-only'].filter((value) => typeof value === 'string' && value.length > 0);
  for (const secret of secrets) {
    // Exact string leaves plus token checks; URL substring matching is not used
    // as host validation (and cannot accidentally bless a superstring host).
    assert.equal(strings([h.storage.history, h.calls.rpc, replies]).some((leaf) => leaf === secret), false);
    assert.equal(serialized([h.storage.history, h.calls.rpc, replies, h.calls.fetch.map(({ body }) => body)])
      .includes('synthetic-local-only'), false, 'webhook token escaped the transport');
  }
}
function assertPost(h, count = 1) {
  assert.equal(h.calls.fetch.length, count, 'eligible closed windows produce one bounded attempt');
  for (const call of h.calls.fetch) {
    assert.equal(call.url, h.env.OPS_ALERT_WEBHOOK_URL);
    assert.equal(call.method, 'POST');
    assert.equal(call.redirect, 'manual', 'Workers supports manual no-follow, not redirect:error');
    assert.match(call.headers['content-type'], /^application\/json(?:;|$)/i);
    assert.ok(call.signal instanceof AbortSignal, 'delivery must carry an abort signal');
    const payload = JSON.parse(call.body);
    assert.equal(payload.embeds.length, 1);
    assert.ok(call.body.length <= 6000);
    assert.ok(strings(payload).some((value) => value === LOGS), 'configured logs link must survive verbatim');
    assert.ok(strings(payload).some((value) => value === SHA), 'producer release identity must survive verbatim');
    assert.equal(call.body.includes(h.env.BUILD_SHA), false, 'consumer SHA must not replace producer identity');
  }
  for (const audit of h.calls.responses) {
    assert.equal(audit.cancelled, true, 'endless response body must be cancelled after bounded consumption');
  }
  privateEffects(h);
}
async function ingestBreach(h, extras = {}) { return h.ingest(input(batch(2, 20, extras))); }
async function independentBreach(h, at) { h.clock.now = at; await ingestBreach(h); await h.close(at + 60000); }

// These controls prove fixtures/harness work independently of the missing runtime;
// they are not a substitute for any runtime acceptance failure below.
test('fixture controls exercise the actual projector, reducer and renderer', () => {
  const projected = projectTailEvent(event({ status: 503 }), PROJECTION_POLICY);
  assert.equal(projected.build_sha, SHA);
  assert.equal(projected.failure_flags.http_5xx, true);
  noCanaries(projected);
  const verdict = evaluateErrorWindow(batch(2), POLICY);
  assert.equal(verdict.verdict, 'breach');
  assert.equal(verdict.counts.total, 20);
  assert.equal(verdict.counts.failed, 2);
  assert.equal(verdict.rate.decimal, '0.1000');
  assert.equal(evaluateErrorWindow(batch(1), POLICY).verdict, 'pass');
  assert.equal(evaluateErrorWindow(batch(19, 19), POLICY).verdict, 'unknown');
  const rendered = renderVerdictAlert(verdict, { logsUrl: LOGS, windowLabel: 'receive-time local window' });
  assert.match(rendered.content, /2\/20/);
  assert.ok(strings(rendered).some((value) => value === SHA));
  noCanaries(rendered);
});

test('local storage double commits/rolls back values and alarms atomically and clones reads', async () => {
  const store = new Storage(new Clock());
  const api = store.api();
  await api.put('control', { count: 1 });
  const copy = await api.get('control'); copy.count = 9;
  assert.equal((await api.get('control')).count, 1);
  await assert.rejects(api.transaction(async (tx) => {
    await tx.put('control', { count: 2 }); await tx.setAlarm(START + 1); throw new Error('rollback control');
  }), /rollback control/);
  assert.equal((await api.get('control')).count, 1);
  assert.equal(await api.getAlarm(), null);
  await api.transaction(async (tx) => { await tx.put('control', { count: 3 }); await tx.setAlarm(START + 10); });
  assert.equal((await api.get('control')).count, 3);
  assert.equal(await api.getAlarm(), START + 10);
});

test('local deadline and endless-body doubles are deterministic and cancelable', async () => {
  const clock = new Clock();
  const controller = new AbortController();
  clock.setTimeout(() => controller.abort(), 5000);
  await clock.tick(4999);
  assert.equal(controller.signal.aborted, false);
  await clock.tick(1);
  assert.equal(controller.signal.aborted, true);
  const reply = response();
  const reader = reply.value.body.getReader();
  const chunk = await reader.read();
  assert.equal(chunk.done, false);
  assert.ok(chunk.value.byteLength > 0);
  await reader.cancel();
  assert.equal(reply.audit.cancelled, true);
});

test('Tail projects healthy, 5xx and exception events into one canonical staging RPC', async () => {
  const h = await harness();
  await h.tail([event(), event({ status: 503 }), event({ outcome: 'exception' })]);
  assert.deepEqual(h.calls.binding, ['api:staging']);
  assert.equal(h.calls.rpc.length, 1);
  const rpc = h.calls.rpc[0];
  assert.deepEqual(Object.keys(rpc).sort(), ['incomplete', 'observations']);
  assert.equal(rpc.incomplete, false);
  assert.equal(rpc.observations.length, 3, 'healthy invocations must remain in the denominator');
  assert.deepEqual(rpc.observations.map((item) => [item.service, item.environment, item.http_status,
    item.failure_flags.http_5xx, item.failure_flags.exception]), [
    ['api', 'staging', 200, false, false], ['api', 'staging', 503, true, false], ['api', 'staging', 200, false, true],
  ]);
  assert.equal(h.calls.project.length, 3, 'the merged projector must actually execute');
  for (const item of rpc.observations) {
    assert.deepEqual(Object.keys(item).sort(), Object.keys(observation()).sort());
    assert.equal(item.build_sha, SHA);
  }
  assert.equal(h.calls.fetch.length, 0, 'Tail ingestion is not a delivery trigger');
  privateEffects(h);
});

test('one invocation with both a 5xx and an exception is counted once, not twice', async () => {
  const h = await harness();
  await h.tail([event({ status: 503, outcome: 'exception' }), ...Array.from({ length: 19 }, () => event())]);
  await h.close();
  assert.equal(h.calls.fetch.length, 0, '1/20 is exactly the threshold even when two failure flags are set');
  assert.ok(h.calls.evaluate.some(({ result }) => result?.counts.total === 20 && result.counts.failed === 1
    && result.counts.by_flag.http_5xx === 1 && result.counts.by_flag.exception === 1));
});

for (const producer of [undefined, null, '', ' ', 7, [], 'nntune-api-production', `${SCRIPT} `]) {
  test(`invalid producer configuration fails closed before binding (${JSON.stringify(producer)})`, async () => {
    const h = await harness({ env: { PRODUCER_SCRIPT_NAME: producer } });
    await h.tail([event({ status: 503 })]);
    assert.equal(h.calls.binding.length, 0);
    assert.equal(h.calls.rpc.length, 0);
    assert.equal(h.calls.fetch.length, 0);
    assert.equal(h.storage.data.size, 0);
    privateEffects(h);
  });
}

test('wrong producer is excluded rather than relabeled or poisoning a complete batch', async () => {
  const h = await harness();
  await h.tail([event(), event({ scriptName: 'unrelated-script', status: 503 })]);
  assert.equal(h.calls.rpc.length, 1);
  assert.equal(h.calls.rpc[0].observations.length, 1);
  assert.equal(h.calls.rpc[0].incomplete, false);
  await h.tail([event({ scriptName: 'unrelated-script', status: 503 })]);
  assert.equal(h.calls.rpc.length, 1, 'wrong-producer-only batches do not open windows');
  privateEffects(h);
});

test('configured-producer null projection forwards an incomplete-only batch', async () => {
  const h = await harness();
  // A malformed root can advertise the producer but cannot be a v1 projection.
  const malformed = Object.assign([], { scriptName: SCRIPT });
  await h.tail([malformed]);
  assert.equal(h.calls.rpc.length, 1);
  assert.deepEqual(h.calls.rpc[0], { observations: [], incomplete: true });
  assert.equal(h.storage.alarm, START + 60000, 'coverage-loss-only windows must be durable/alarmed');
  await ingestBreach(h);
  await h.close();
  assert.equal(h.calls.fetch.length, 0, 'lost coverage cannot become a sampled-rate alert');
  privateEffects(h);
});

test('oversized Tail batches inspect <=256 entries and never silently evaluate the prefix', async () => {
  const h = await harness();
  let beyondCapReads = 0;
  const events = Array.from({ length: 257 }, () => event({ status: 503 }));
  Object.defineProperty(events, '256', { get() { beyondCapReads += 1; throw new Error(CANARIES[0]); } });
  await h.tail(events);
  assert.equal(beyondCapReads, 0, 'must not inspect event 257');
  assert.equal(h.calls.rpc.length, 1);
  assert.equal(h.calls.rpc[0].incomplete, true);
  assert.ok(h.calls.rpc[0].observations.length <= 256);
  await h.close();
  assert.equal(h.calls.fetch.length, 0);
  privateEffects(h);
});

test('oversized batches forward lost coverage even when all inspected producers are excluded', async () => {
  const h = await harness();
  await h.tail(Array.from({ length: 257 }, () => event({ scriptName: 'wrong' })));
  assert.deepEqual(h.calls.rpc, [{ observations: [], incomplete: true }]);
  await ingestBreach(h);
  await h.close();
  assert.equal(h.calls.fetch.length, 0);
});

test('two Tail calls accumulate durably; the real reducer and renderer drive the POST', async () => {
  const h = await harness();
  await h.tail([event({ status: 503 }), ...Array.from({ length: 9 }, () => event())]);
  assert.ok(h.storage.data.size > 0, 'observations must persist before ingestion resolves');
  assert.equal(h.storage.alarm, START + 60000, 'earliest alarm must persist before ingestion resolves');
  await h.restart();
  await h.tail([event({ outcome: 'exception' }), ...Array.from({ length: 9 }, () => event())]);
  await h.close();
  assertPost(h);
  const reduced = h.calls.evaluate.find(({ result }) => result?.counts.total === 20);
  assert.ok(reduced, 'accumulated window must go through the real reducer');
  assert.equal(reduced.result.counts.failed, 2);
  assert.equal(reduced.result.rate.decimal, '0.1000');
  assert.deepEqual(reduced.args[1], POLICY, 'default policy uses the actual frozen rational/map schema');
  assert.equal(h.calls.render.length, 1);
  assert.deepEqual(JSON.parse(h.calls.fetch[0].body), h.calls.render[0].result);
  assert.equal(h.calls.render[0].args[1].logsUrl, LOGS);
  assert.deepEqual(Object.keys(h.calls.render[0].args[1]).sort(), ['logsUrl', 'windowLabel']);
  const label = h.calls.render[0].args[1].windowLabel;
  assert.match(label, /rece(?:ipt|ive|ived)/i, 'label must honestly identify DO receipt time');
  assert.equal(serialized(h.calls.render).includes('synthetic-local-only'), false, 'secret endpoint must never reach renderer');
  await h.restart(); await h.close(START + 120000); await h.close(START + 180000);
  assertPost(h);
  assert.equal(h.storage.alarm, null, 'idle DO must stop rearming');
  bounded(h);
});

for (const [name, failed, total, posts] of [
  ['healthy', 0, 20, 0], ['below floor', 19, 19, 0], ['exact threshold', 1, 20, 0],
  ['above threshold', 2, 20, 1], ['exact larger denominator', 2, 40, 0], ['above larger denominator', 3, 40, 1],
]) {
  test(`default reducer window: ${name}`, async () => {
    const h = await harness();
    await h.ingest(input(batch(failed, total)));
    await h.close();
    assert.equal(h.calls.fetch.length, posts);
    assert.ok(h.calls.evaluate.some(({ result }) => result?.counts.total === total && result.counts.failed === failed),
      'complete window must use real reducer with the full denominator');
    if (posts) assertPost(h);
    assert.equal(h.storage.alarm, null);
    privateEffects(h);
  });
}

test('RPC rebuilds a whitelist and drops unexpected properties at every retained level', async () => {
  const h = await harness();
  const rows = batch(2).map((row) => ({ ...row, raw: CANARIES[0], request: { email: CANARIES[1] },
    failure_flags: { ...row.failure_flags, stack: CANARIES[3] } }));
  const reply = await h.ingest({ ...input(rows), headers: CANARIES[2] });
  privateEffects(h, [reply]);
  await h.restart(); await h.close();
  assertPost(h);
  const payloadStrings = strings(JSON.parse(h.calls.fetch[0].body));
  assert.ok(payloadStrings.some((value) => value === 'GET /health'), 'renderer must retain canonical route grouping');
  assert.ok(payloadStrings.some((value) => value === 'upstream'), 'renderer must retain canonical error-class grouping');
  assert.match(JSON.parse(h.calls.fetch[0].body).content, /2\/20/);
  for (const call of h.calls.evaluate) {
    for (const row of call.args[0]) {
      assert.deepEqual(Object.keys(row).sort(), Object.keys(observation()).sort());
      assert.deepEqual(Object.keys(row.failure_flags).sort(), ['exceeded_cpu', 'exceeded_memory', 'exception', 'http_5xx']);
    }
  }
});

const invalidRecords = [
  ['null', null], ['wrong schema', observation(true, { schema_version: 2 })],
  ['wrong service', observation(true, { service: 'other' })],
  ['wrong environment', observation(true, { environment: 'production' })],
  ['nonboolean flag', observation(true, { failure_flags: { http_5xx: 'yes', exception: false, exceeded_cpu: false, exceeded_memory: false } })],
  ['missing flags', observation(true, { failure_flags: {} })],
  ['invalid outcome', observation(true, { outcome: CANARIES[0] })],
  ['invalid HTTP status', observation(true, { http_status: 600 })],
  ['contradictory 5xx flag', observation(true, { http_status: 200 })],
  ['mismatched producer release', observation(true, { release: OTHER_SHA })],
  ['raw route', observation(true, { route: `GET /health?email=${CANARIES[1]}` })],
  ['raw error class', observation(true, { error_class: CANARIES[3] })],
];
for (const [name, row] of invalidRecords) {
  test(`invalid RPC record marks the entire receive window incomplete: ${name}`, async () => {
    const h = await harness();
    await ingestBreach(h);
    const reply = await h.ingest(input([row]));
    await h.restart(); await h.close();
    assert.equal(h.calls.fetch.length, 0, 'must not alert on retained valid prefix after rejecting a record');
    incompleteEffects(h, [reply]);
    assert.equal(h.storage.alarm, null);
  });
}

for (const bad of [null, [], {}, { observations: {}, incomplete: false }, { observations: batch(2), incomplete: 'false' }]) {
  test(`malformed RPC envelope cannot yield a healthy or breach verdict (${serialized(bad)?.slice(0, 48)})`, async () => {
    const h = await harness();
    const reply = await h.ingest(bad);
    await h.close();
    assert.equal(h.calls.fetch.length, 0);
    incompleteEffects(h, [reply]);
  });
}

test('explicit incompleteness survives reconstruction and suppresses even an otherwise eligible breach', async () => {
  const h = await harness();
  await h.ingest(input([], true));
  await h.restart(); await ingestBreach(h); await h.close();
  assert.equal(h.calls.fetch.length, 0);
  incompleteEffects(h);
  assert.equal(h.storage.alarm, null);
});

for (const [offset, posts] of [[59999, 1], [60000, 0]]) {
  test(`receive-time membership is half-open at ${offset}ms, not Tail timestamps`, async () => {
    const h = await harness();
    await h.tail(Array.from({ length: 19 }, () => event({ status: 503 })));
    h.clock.now = START + offset;
    const late = event(); late.eventTimestamp = START - 86400000;
    await h.tail([late]);
    await h.close(START + 60000);
    assert.equal(h.calls.fetch.length, posts, 'deadline receipt must never fill the previous denominator');
    await h.close(START + 120000);
    assert.equal(h.calls.fetch.length, posts);
    if (posts) assertPost(h);
    assert.equal(h.storage.alarm, null);
  });
}

test('the first receipt starts the window, not the nearest wall-clock minute', async () => {
  const first = START + 12345;
  const h = await harness({ now: first });
  await h.tail([]);
  assert.equal(h.calls.rpc.length, 0);
  assert.equal(h.storage.alarm, null);
  await ingestBreach(h);
  assert.equal(h.storage.alarm, first + 60000);
  await h.close(START + 60000);
  assert.equal(h.calls.fetch.length, 0, 'calendar-minute boundary is not this window deadline');
  assert.equal(h.storage.alarm, first + 60000);
  await h.close(first + 60000);
  assertPost(h);
});

test('early alarms preserve pending state and earliest deadline; delayed alarms label the original receipt window', async () => {
  const h = await harness();
  await ingestBreach(h);
  await h.close(START + 59999);
  assert.equal(h.calls.fetch.length, 0);
  assert.equal(h.storage.alarm, START + 60000);
  await h.restart(); await h.close(START + 1800000);
  assertPost(h);
  const label = h.calls.render[0].args[1].windowLabel;
  // Format is not frozen: ISO/time-of-day or epoch-ms boundaries are accepted.
  assert.ok(label.includes('00:00') || label.includes(String(START)), 'missing original receipt-window start');
  assert.ok(label.includes('00:01') || label.includes(String(START + 60000)), 'missing half-open receipt-window end');
  assert.equal(h.storage.alarm, null);
  await h.restart(); await settle(h.alarm()); await settle(h.alarm());
  assert.equal(h.calls.evaluate.length, 1, 'closed window must be consumed at most once durably');
  assertPost(h);
});

test('a full but not overflowing 2048-observation window stays complete under the per-key bound', async () => {
  const h = await harness();
  const wide = { route: `GET /${'a'.repeat(123)}`, error_class: 'a'.repeat(64) };
  for (let i = 0; i < 8; i += 1) await h.ingest(input(batch(256, 256, wide)));
  await h.restart(); await h.close();
  assertPost(h);
  assert.ok(h.calls.evaluate.some(({ result }) => result?.counts.total === 2048 && result.counts.failed === 2048));
  bounded(h);
  assert.equal(h.storage.alarm, null);
});

test('saturated observation storage is sharded/bounded and cannot alert from a retained prefix', async () => {
  const h = await harness();
  const wide = { route: `GET /${'a'.repeat(123)}`, error_class: 'a'.repeat(64) };
  for (let i = 0; i < 8; i += 1) await h.ingest(input(batch(256, 256, wide)));
  bounded(h);
  // Healthy overflow is denominator loss, not permission to use 2048 failures.
  await h.ingest(input(batch(0, 256, wide)));
  await h.restart(); await h.close();
  assert.equal(h.calls.fetch.length, 0);
  incompleteEffects(h);
  bounded(h);
  assert.ok(Buffer.byteLength(serialized(h.storage.snapshot())) < 8192, 'closed observation shards must be cleaned');
  assert.equal(h.storage.alarm, null);
});

test('two complete delayed windows keep distinct denominators and share one global cooldown', async () => {
  const h = await harness();
  await h.ingest(input(batch(2)));
  h.clock.now = START + 60000;
  await h.ingest(input(batch(0, 200)));
  await h.restart(); await h.close(START + 120000);
  assertPost(h);
  const counts = h.calls.evaluate.map(({ result }) => [result.counts.total, result.counts.failed]);
  assert.deepEqual(counts.sort((a, b) => a[0] - b[0]), [[20, 2], [200, 0]], 'never mix receive buckets');
  assert.equal(h.storage.alarm, null);
  bounded(h);
});

test('a third delayed receive bucket preserves coverage loss if two pending windows prevent retention', async () => {
  const h = await harness();
  await h.ingest(input(batch(0)));
  h.clock.now = START + 60000; await h.ingest(input(batch(0)));
  const before = serialized(h.storage.snapshot());
  const alreadyClosed = h.calls.evaluate.length;
  h.clock.now = START + 120000; await h.ingest(input(batch(20)));
  assert.notEqual(serialized(h.storage.snapshot()), before, 'capacity refusal must leave durable evidence, not silently drop receipts');
  // Closing elapsed buckets proactively is legal. Otherwise the third bucket
  // lost coverage and subsequent ingress in it must not resurrect a sampled rate.
  const overflowed = alreadyClosed === 0 && h.calls.evaluate.length === 0;
  await h.restart(); await h.close(START + 120001);
  await h.ingest(input(batch(20))); await h.close(START + 180000);
  if (overflowed) {
    assert.equal(h.calls.fetch.length, 0, 'overflow bucket must stay incomplete after capacity becomes available');
    await independentBreach(h, START + 180000);
    assertPost(h, 1); // Coverage loss must not permanently disable later clean windows.
  }
  assert.ok(h.calls.evaluate.length <= 4, 'retain at most two pending windows, not an unbounded backlog');
  bounded(h);
  assert.equal(h.storage.alarm, null);
});

// Lost-coverage fixture: two healthy pending windows, then a breach receipt at
// START+120000 that no pending window can retain (coverage lost until +180000).
async function tombstone(h) {
  await h.ingest(input(batch(0)));
  h.clock.now = START + 60000; await h.ingest(input(batch(0)));
  h.clock.now = START + 120000; await h.ingest(input(batch(20)));
}

test('a consumed closed window leaves no retained observation rows in durable storage', async () => {
  const h = await harness();
  await ingestBreach(h);
  assert.equal(recordCount(h.storage.snapshot()), 20, 'control: pending observations are durable');
  await h.restart(); await h.close();
  assertPost(h);
  assert.equal(recordCount(h.storage.snapshot()), 0, 'closed-window observations must be deleted once consumed');
  assert.equal(h.storage.alarm, null);
});

test('an expired lost-coverage tombstone is cleared and stops arming the alarm', async () => {
  const h = await harness();
  await tombstone(h);
  await h.restart(); await h.close(START + 120000);
  assert.notEqual(h.storage.alarm, null, 'control: unexpired lost coverage stays durable/alarmed');
  await h.close(START + 180000);
  assert.equal(h.storage.alarm, null, 'an alarm must not stay armed at an already-reached tombstone deadline');
  await h.restart(); await settle(h.alarm());
  assert.equal(h.storage.alarm, null);
  assert.equal(h.calls.fetch.length, 0);
  bounded(h);
});

test('receipts inside a lost-coverage tombstone cannot open a complete window; its half-open end can', async () => {
  const h = await harness();
  await tombstone(h);
  // Both pending windows are consumed, so capacity is free while coverage is still lost.
  await h.restart(); await h.close(START + 120000);
  h.clock.now = START + 120001; await ingestBreach(h);
  // Delayed alarm: ingress exactly at the tombstone end arrives before it runs.
  h.clock.now = START + 180000; await ingestBreach(h);
  await h.restart(); await h.close(START + 240000);
  assertPost(h);
  const breaches = h.calls.evaluate.filter(({ result }) => result?.counts.failed > 0)
    .map(({ result }) => [result.counts.total, result.counts.failed]);
  assert.deepEqual(breaches, [[20, 2]], 'only the post-tombstone receipt may form a complete window');
  assert.equal(h.storage.alarm, null);
  bounded(h);
});

test('a 257-observation RPC batch is incomplete; its complete 256-row prefix cannot alert', async () => {
  const h = await harness();
  const reply = await h.ingest(input(batch(257, 257)));
  await h.restart(); await h.close();
  assert.equal(h.calls.fetch.length, 0, 'over-cap RPC batches must not alert from a retained prefix');
  assert.equal(h.calls.evaluate.some(({ result }) => result?.counts.total === 256), false);
  incompleteEffects(h, [reply]);
  assert.equal(h.storage.alarm, null);
  await independentBreach(h, START + 60000);
  assertPost(h); // The cap marks only that receive window incomplete.
});

test('alarm reservation is durable before fetch; ingress can proceed while network I/O is pending', async () => {
  const gate = deferred();
  const entered = deferred();
  const h = await harness({ deliver: async () => { entered.resolve(); return gate.promise; } });
  await ingestBreach(h);
  h.clock.now = START + 60000;
  const alarm = h.alarm();
  await settle(entered.promise, 'eligible alarm never attempted delivery');
  assert.ok(h.calls.fetch[0].durable.values.length > 0, 'pre-I/O delivery reservation must be durable');
  await settle(h.ingest(input(batch(2))), 'network I/O must not block new ingress');
  assert.equal(h.storage.alarm, START + 120000, 'new window must be alarmed before acknowledgment');
  // Reconstruct with shared durable storage before resolving the first POST.
  await h.restart(); await settle(h.alarm());
  assert.equal(h.calls.fetch.length, 1, 'duplicate alarm/restart during delivery cannot double-attempt');
  const reply = response(); h.calls.responses.push(reply.audit); gate.resolve(reply.value);
  await settle(alarm);
  await h.close(START + 120000);
  assert.equal(h.calls.fetch.length, 1, 'new eligible window within cooldown must not attempt');
  assert.ok(h.calls.evaluate.some(({ result }) => result?.counts.total === 20 && result.counts.failed === 2));
  assert.equal(h.calls.evaluate.length, 2, 'network completion must not erase appended window');
  assert.equal(h.storage.alarm, null);
  assertPost(h);
});

test('simultaneous ingress cannot lose observations before atomic window persistence', async () => {
  const h = await harness();
  await Promise.all([h.ingest(input(batch(1, 10))), h.ingest(input(batch(1, 10)))]);
  await h.restart(); await h.close();
  assertPost(h);
  assert.ok(h.calls.evaluate.some(({ result }) => result?.counts.total === 20 && result.counts.failed === 2));
});

for (const kind of ['success', 'http-429', 'http-503', 'redirect', 'network']) {
  test(`global cooldown survives ${kind}, duplicate alarms and reconstruction, then expires normally`, async () => {
    const audits = [];
    const h = await harness({ deliver: async (call) => {
      if (kind === 'network') throw new Error(`${CANARIES[3]} ${CANARIES[0]} ${call.url}`);
      const status = { success: 200, 'http-429': 429, 'http-503': 503, redirect: 302 }[kind];
      const reply = response(status); audits.push(reply.audit); return reply.value;
    } });
    await ingestBreach(h); await h.close(START + 60000);
    assert.equal(h.calls.fetch.length, 1);
    await h.restart(); await Promise.all([h.alarm(), h.alarm()]);
    await h.clock.tick(60000); // Execute delayed retries too, not merely alarm re-entry.
    assert.equal(h.calls.fetch.length, 1, 'no scheduled retry after a completed attempt');
    await independentBreach(h, START + 120000);
    await h.restart();
    await h.clock.tick(119999);
    // This window closes one millisecond before reservation+300000.
    await independentBreach(h, START + 299999);
    assert.equal(h.calls.fetch.length, 1, 'no retries or independent attempts before global cooldown expiry');
    // First reservation was at START+60000, so expiry is START+360000.
    h.clock.now = START + 360000; await ingestBreach(h); await h.close(START + 420000);
    assert.equal(h.calls.fetch.length, 2, 'later independent breach may attempt after cooldown expiry');
    for (const audit of audits) {
      assert.equal(audit.cancelled, true, 'endless error/success response body must be cancelled');
    }
    assertPost(h, 2);
    bounded(h);
  });
}

// Fetch is local-only: return each redirect unchanged, as Workers manual mode
// does. Follow mode, an explicit Location fetch or a retry must fail acceptance.
for (const [status, location] of [
  [301, 'https://redirect.example.invalid/collect'],
  [302, 'https://discord.com/api/webhooks/456/synthetic-other-target'],
  [303, '/redirect-target'],
  [307, 'http://redirect.example.invalid/collect'],
  [308, 'https://redirect.example.invalid/permanent'],
]) {
  test(`HTTP ${status} with Location is cancelled without following or retrying`, async () => {
    const h = await harness({ deliver: async () => {
      const reply = response(status, { Location: location });
      h.calls.responses.push(reply.audit);
      return reply.value;
    } });
    await ingestBreach(h); await h.close(START + 60000);
    assert.equal(h.calls.fetch.length, 1, 'Location must not trigger another request');
    assert.equal(h.calls.responses[0].cancelled, true, 'redirect body must be cancelled');
    await h.restart(); await Promise.all([h.alarm(), h.alarm()]);
    await h.clock.tick(5000);
    // Close another eligible window 1ms before the durable 300000ms cooldown.
    await independentBreach(h, START + 299999);
    assertPost(h);
    bounded(h);
  });
}

for (const [gap, posts] of [[299999, 1], [300000, 2]]) {
  test(`cooldown boundary is exactly 300000ms between attempts (gap ${gap})`, async () => {
    const h = await harness();
    await ingestBreach(h); await h.close(START + 60000);
    await h.restart();
    await independentBreach(h, START + gap);
    assertPost(h, posts);
    assert.equal(h.calls.fetch[0].at, START + 60000);
    if (posts === 2) assert.equal(h.calls.fetch[1].at - h.calls.fetch[0].at, 300000);
  });
}

test('a hung delivery is aborted within 5000ms and cannot leak or retry after timeout', async () => {
  const entered = deferred();
  const h = await harness({ deliver: async (call) => {
    entered.resolve(call);
    return new Promise((resolve, reject) => {
      const abort = () => reject(new Error(`${CANARIES[3]} ${call.url}`));
      if (call.signal.aborted) abort(); else call.signal.addEventListener('abort', abort, { once: true });
    });
  } });
  await ingestBreach(h); h.clock.now = START + 60000;
  const alarm = h.alarm();
  const call = await settle(entered.promise);
  await h.clock.tick(5000);
  assert.equal(call.signal.aborted, true, 'delivery must have a <=5000ms abort deadline');
  await settle(alarm, 'aborted delivery must settle without an exception');
  await h.restart(); await settle(h.alarm());
  await independentBreach(h, START + 120000);
  assert.equal(h.calls.fetch.length, 1, 'timeout consumes cooldown, never triggers a retry');
  privateEffects(h);
});

const invalidWebhooks = [
  undefined, '', 'not a URL',
  'http://discord.com/api/webhooks/123/synthetic-local-only',
  'https://discord.com.evil.invalid/api/webhooks/123/synthetic-local-only',
  'https://evil-discord.com/api/webhooks/123/synthetic-local-only',
  'https://canary@discord.com/api/webhooks/123/synthetic-local-only',
  'https://discord.com:8443/api/webhooks/123/synthetic-local-only',
  'https://discord.com/api/webhooks/123/synthetic-local-only?wait=true',
  'https://discord.com/api/webhooks/123/synthetic-local-only#fragment',
  'https://discord.com/api/webhooks/not-numeric/synthetic-local-only',
  'https://discord.com/api/webhooks/123/',
  'https://discord.com/api/webhooks/123/synthetic-local-only/extra',
];
for (const [index, webhook] of invalidWebhooks.entries()) {
  test(`invalid webhook endpoint ${index} disables delivery but not durable cleanup`, async () => {
    const h = await harness({ env: { OPS_ALERT_WEBHOOK_URL: webhook } });
    await ingestBreach(h); await h.close(); await h.restart(); await settle(h.alarm());
    assert.equal(h.calls.fetch.length, 0);
    assert.equal(h.storage.alarm, null);
    assert.ok(Buffer.byteLength(serialized(h.storage.snapshot())) < 8192);
    privateEffects(h);
  });
}
for (const logs of [undefined, '', ' ', 'https://?x=1', 'javascript:alert(1)', 'https://bad host.invalid']) {
  test(`invalid logs link disables delivery but not cleanup (${JSON.stringify(logs)})`, async () => {
    const h = await harness({ env: { LOGS_URL: logs } });
    await ingestBreach(h); await h.close();
    assert.equal(h.calls.fetch.length, 0);
    assert.equal(h.storage.alarm, null);
    assert.ok(Buffer.byteLength(serialized(h.storage.snapshot())) < 8192);
    privateEffects(h);
  });
}

test('unknown producer identity is never replaced by consumer build metadata', async () => {
  const h = await harness();
  await h.ingest(input(batch(2, 20, { build_sha: 'unknown', release: 'unknown', route: 'unknown', error_class: 'unknown' })));
  await h.close();
  // The existing renderer can render an honest unknown identity. Suppression is
  // also safe; inventing a release or substituting the consumer's SHA is not.
  for (const { body } of h.calls.fetch) {
    assert.match(body, /unknown/);
    assert.equal(body.includes(h.env.BUILD_SHA), false);
    assert.equal(/[a-f0-9]{40}/.test(body), false);
  }
  privateEffects(h);
});

test('public HTTP is always 404 and cannot invoke binding, storage or network', async () => {
  const h = await harness();
  const before = h.storage.snapshot();
  const readsBefore = h.storage.accesses.length;
  const writesBefore = h.storage.history.length;
  for (const method of ['GET', 'POST', 'PUT', 'DELETE']) {
    const reply = await h.publicFetch(new Request('https://consumer.example.invalid/ingest', {
      method, ...(method === 'GET' ? {} : { body: serialized({ ...input(batch(20)), secret: CANARIES[0] }) }),
    }));
    assert.equal(reply.status, 404);
    noCanaries(await reply.text());
  }
  assert.deepEqual(h.storage.snapshot(), before);
  assert.equal(h.calls.binding.length, 0);
  assert.equal(h.calls.fetch.length, 0);
  assert.equal(h.storage.history.length, writesBefore, 'public fetch must not write storage');
  assert.equal(h.storage.accesses.length, readsBefore, 'public fetch must not read storage');
  privateEffects(h);
});

test('example configuration is an inactive local consumer with DO binding/migration and tracing disabled', async () => {
  const url = new URL('../wrangler.toml.example', import.meta.url);
  let source;
  try { source = await readFile(url, 'utf8'); }
  catch (error) {
    if (error.code === 'ENOENT') assert.fail('missing runtime contract: workers/observability/wrangler.toml.example must be inactive by default');
    throw error;
  }
  const parsed = spawnSync('python3', ['-c', 'import json,sys,tomllib; print(json.dumps(tomllib.loads(sys.stdin.read())))'],
    { input: source, encoding: 'utf8', timeout: 2000 });
  assert.equal(parsed.status, 0, 'example must parse as TOML');
  const config = JSON.parse(parsed.stdout);
  assert.equal(new URL(config.main, url).href, new URL('../runtime.mjs', import.meta.url).href);
  assert.equal(config.workers_dev, false);
  assert.equal(config.observability?.enabled, false);
  assert.notEqual(config.observability?.traces?.enabled, true);
  assert.ok(config.durable_objects?.bindings?.some((binding) => binding.name === 'ALERT_WINDOWS' && binding.class_name === 'ErrorRateAlerts'));
  assert.ok(config.migrations?.some((migration) => [...(migration.new_classes ?? []), ...(migration.new_sqlite_classes ?? [])].includes('ErrorRateAlerts')));
  function inactive(value) {
    if (!value || typeof value !== 'object') return;
    for (const [key, child] of Object.entries(value)) {
      assert.equal(['account_id', 'zone_id', 'route', 'routes', 'tail_consumers', 'tail_consumer', 'crons', 'OPS_ALERT_WEBHOOK_URL'].includes(key), false,
        'example must not configure live accounts, routes, attachments, triggers or secret values');
      inactive(child);
    }
  }
  inactive(config);
  assert.equal(config.env, undefined, 'source example must not add deployment environments');
  assert.equal(/https?:\/\/[^\s]*discord[^\s]*\/webhooks\//i.test(source), false, 'example must document only the secret binding NAME');
});
