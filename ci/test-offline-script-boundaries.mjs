import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { finalSmokeFixture, recoveryFixture } from './fixtures/production-worker-transport.mjs';
import { admitRecovery, intentFile } from './production-maintenance/recovery-evidence.mjs';
import worker from './production-maintenance/worker.mjs';
import { evaluateHealthWindow } from '../workers/observability/evaluate-health-window.mjs';
import { renderHealthAlert } from '../workers/observability/render-health-alert.mjs';
import provider from '../.github/scripts/private_trial_provider_preflight.cjs';
import cleanup from '../.github/scripts/staging_trial_cleanup.cjs';

const checkedAt = '2026-10-01T12:00:00Z';
const now = () => Date.parse(checkedAt);
const fixture = { checkedAt, now, sha: 'a'.repeat(40), workerId: 'b'.repeat(32),
  version: '12345678-1234-1234-1234-123456789abc' };

test('rollback rejects invalid context and stale or superseded recovery evidence', async (t) => {
  const transport = finalSmokeFixture(t, fixture);
  const { run, calls, rows, id, seed } = transport;
  await assert.rejects(run('check-seed', { DOMAIN: 'staging..example.com' }), /rollback context/);
  await assert.rejects(run('record', {}, () => NaN), /Stale production rollback custody/);
  assert.deepEqual(calls, []);
  await run('record');
  transport.shift();
  const { fresh, steps, evidence } = recoveryFixture(t, transport, 'success');
  const env = fresh('13');
  steps.push({ name: 'Restore recorded maintenance after failed final smoke', status: 'in_progress',
    conclusion: 'skipped', number: 30 });
  assert.throws(() => admitRecovery(env, evidence), /Unverified production recovery source/);
  assert.equal(existsSync(join(env.RUNNER_TEMP, intentFile)), false);
  steps[2].status = 'completed';
  admitRecovery(env, evidence);
  const module = seed.modules[0];
  const contentType = module.content_type;
  module.content_type = 'text/plain';
  await assert.rejects(run('prepare-recovery', env), /Maintenance version drifted/);
  module.content_type = contentType;
  await run('prepare-recovery', env);
  rows.unshift({ ...rows[0], id: id(8), created_on: new Date(now() + 8_000).toISOString() });
  await assert.rejects(run('recover', { ...env, CLAIM_VERIFIED: 'true' }), /newer or foreign/);
  assert.ok(calls.every((method) => method === 'GET'));
  assert.equal((await worker.fetch(new Request('https://fixture.invalid/', { method: 'POST' }))).status, 503);
});

test('health windows and alerts reject evidence they cannot represent safely', () => {
  const policy = { service: 'api', environment: 'staging', red_for_ms: 1, min_samples: 2 };
  const samples = [{ t: 1, ok: false }, { t: 2, ok: false }];
  const verdict = evaluateHealthWindow(samples, policy);
  assert.equal(verdict.verdict, 'breach');
  assert.equal(evaluateHealthWindow(new Array(100_001), policy), null);
  for (const invalid of [Object.create(samples[1]), { t: Number.MAX_SAFE_INTEGER + 1, ok: false }]) {
    assert.equal(evaluateHealthWindow([samples[0], invalid], policy).verdict, 'unknown');
  }
  const context = { windowLabel: 'test window', logsUrl: 'https://logs.example.invalid/' };
  assert.ok(renderHealthAlert(verdict, context));
  assert.equal(renderHealthAlert({ ...verdict, counts: { ...verdict.counts, red: 1 } }, context), null);
  assert.equal(renderHealthAlert(verdict, { ...context, windowLabel: 'label\ud800' }), null);
  for (const logsUrl of ['https://user:secret@logs.example.invalid/', 'https://\\logs.example.invalid/',
    'https://logs.example.invalid/\u0000']) {
    assert.equal(renderHealthAlert(verdict, { ...context, logsUrl }), null);
  }
});

test('provider preflight refuses ambiguous replies and isolates its fetch override', async (t) => {
  const dir = mkdtempSync(join(tmpdir(), 'offline-provider-'));
  t.after(() => rmSync(dir, { recursive: true }));
  const bundle = join(dir, 'fixture.cjs');
  writeFileSync(bundle, `exports.qualify_disposable_version = async (endpoint, method) => {
    await fetch(new Request(endpoint, { method })); return 'qualified';
  };`);
  const originalFetch = globalThis.fetch;
  const endpoint = 'https://fixture.invalid/';
  const run = (method, send) => provider.qualify(bundle, endpoint, method, '', '', send);
  const reply = (headers) => Object.defineProperty(new Response(null, { headers }), 'url', { value: endpoint });
  const first = run('HEAD', async () => reply({}));
  assert.equal(await run('HEAD', async () => assert.fail('overlapping probe')), 'unconfirmed');
  assert.equal(await first, 'qualified');
  assert.equal(await run('HEAD', async () => reply({ 'transfer-encoding': 'chunked' })), 'unconfirmed');
  let calls = 0;
  assert.equal(await run('POST', async () => { calls++; return reply({}); }), 'unconfirmed');
  assert.equal(calls, 0);
  assert.equal(globalThis.fetch, originalFetch);
});

test('cleanup requires an exact status and byte receipt', async () => {
  assert.equal(await cleanup.cleanupVerdict(new Response('confirmed')), 'confirmed');
  for (const response of [new Response('\ufeffconfirmed'), new Response('confirmed\n'),
    new Response('confirmed', { status: 409 }), new Response('confirm')]) {
    assert.equal(await cleanup.cleanupVerdict(response), 'unconfirmed');
  }
});
