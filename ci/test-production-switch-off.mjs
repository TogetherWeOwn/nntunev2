import assert from 'node:assert/strict';
import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import { finalSmokeFixture, recoveryFixture } from './fixtures/production-worker-transport.mjs';
import { intentFile } from './production-maintenance/recovery-evidence.mjs';

const checkedAt = '2026-10-01T12:00:00Z';
const now = () => Date.parse(checkedAt) + 60_000;
const version = '12345678-1234-1234-1234-123456789abc';
const fixture = { checkedAt, now, sha: 'a'.repeat(40), workerId: 'b'.repeat(32), version };

test('maintenance remains active when the candidate receipt cannot identify a distinct version', async (t) => {
  const { calls, rows, versions, temp, run } = finalSmokeFixture(t, fixture);
  await run('check-seed');
  const baseline = structuredClone(rows);
  const candidate = versions[1];
  for (const id of [undefined, version]) {
    // Keep the exact attempt tag on a receipt that omits the UUID or claims the seed UUID.
    versions[1] = { ...candidate, id };
    await assert.rejects(run('record'), /Candidate tag did not resolve uniquely/);
    assert.equal(existsSync(join(temp, intentFile)), false);
    assert.deepEqual(rows, baseline);
  }
  versions[1] = candidate;
  await run('record');
  assert.equal(existsSync(join(temp, intentFile)), true);
  assert.deepEqual(rows, baseline);
  assert.ok(calls.every((method) => method === 'GET'));
});

test('interrupted recovery retries only with the sealed candidate identity', async (t) => {
  const transport = finalSmokeFixture(t, fixture);
  await transport.run('record');
  transport.shift();
  const { enter, fresh } = recoveryFixture(t, transport, 'timed_out');
  const rollout = structuredClone(transport.rows);
  const candidate = transport.versions[1];
  const interrupted = fresh('13');
  transport.fail.nextRead = true;
  await assert.rejects(enter(interrupted), /Recorded candidate version: Cloudflare request/);
  assert.equal(existsSync(join(interrupted.RUNNER_TEMP, 'production-final-allocation.sealed')), false);

  const driftedReceipts = [
    { ...candidate, id: transport.id(9) },
    { ...candidate, annotations: { 'workers/tag': `${fixture.sha}-11-1` } },
    { ...candidate, annotations: {} },
  ];
  for (const [i, receipt] of driftedReceipts.entries()) {
    transport.versions[1] = receipt;
    const attempt = fresh(String(14 + i));
    await assert.rejects(enter(attempt), /Candidate version drifted/);
    assert.equal(existsSync(join(attempt.RUNNER_TEMP, 'production-final-allocation.sealed')), false);
    assert.deepEqual(transport.rows, rollout);
  }
  assert.ok(transport.calls.every((method) => method === 'GET'));

  transport.versions[1] = candidate;
  assert.equal(await enter(fresh('17')), 'Recorded maintenance allocation verified');
  assert.deepEqual(transport.rows[0].versions, [{ version_id: version, percentage: 100 }]);
  assert.deepEqual(transport.rows.slice(1), rollout);
  assert.equal(await enter(fresh('18')), 'Recorded maintenance allocation already verified');
  assert.equal(transport.calls.filter((method) => method === 'POST').length, 1);
});

test('operator switch-off requires complete readback of the pinned maintenance allocation', async (t) => {
  const cases = [
    { name: 'all traffic serves the pinned maintenance version' },
    {
      name: 'the pinned maintenance version is reported under a different deployment ID',
      changeReadback(history) {
        history.result.deployments[0].id = '00000000-0000-0000-0000-000000000006';
      },
      refusal: /Restore result unresolved/,
    },
    {
      name: 'the acknowledged deployment serves a different version',
      changeReadback(history, candidate) {
        history.result.deployments[0].versions = [{ version_id: candidate, percentage: 100 }];
      },
      refusal: /Restore result unresolved/,
    },
    {
      name: 'the candidate still receives some traffic',
      changeReadback(history, candidate) {
        const { versions } = history.result.deployments[0];
        versions[0].percentage = 95;
        versions.push({ version_id: candidate, percentage: 5 });
      },
      refusal: /Restore result unresolved/,
    },
    {
      name: 'the only reported version has an incomplete allocation',
      changeReadback(history) {
        history.result.deployments[0].versions[0].percentage = 95;
      },
      refusal: /Restore result unresolved/,
    },
    {
      name: 'the deployment history readback is incomplete',
      changeReadback(history) {
        history.result_info.total_count += 1;
      },
      refusal: /deployments on a complete first page/,
    },
  ];

  for (const { name, changeReadback, refusal } of cases) {
    await t.test(name, async (t) => {
      const transport = finalSmokeFixture(t, {
        ...fixture,
        deploymentReadback(receipt) {
          const history = structuredClone(receipt);
          changeReadback?.(history, transport.candidate);
          return history;
        },
      });
      await transport.run('record');
      transport.shift();
      const { candidate } = transport;
      assert.deepEqual(transport.rows[0].versions, [{ version_id: candidate, percentage: 100 }]);

      const { enter, fresh } = recoveryFixture(t, transport, 'success');
      const result = enter(fresh('13'));
      if (refusal) await assert.rejects(result, refusal);
      else assert.equal(await result, 'Recorded maintenance allocation verified');

      assert.deepEqual(transport.rows[0].versions, [
        { version_id: transport.custody.maintenance.version, percentage: 100 },
      ]);
      assert.equal(transport.calls.filter((method) => method === 'POST').length, 1);
      assert.deepEqual(transport.calls.slice(-4), ['POST', 'GET', 'GET', 'GET']);
    });
  }
});
