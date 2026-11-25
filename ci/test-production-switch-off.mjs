import assert from 'node:assert/strict';
import { test } from 'node:test';
import { finalSmokeFixture, recoveryFixture } from './fixtures/production-worker-transport.mjs';

const checkedAt = '2026-10-01T12:00:00Z';
const now = () => Date.parse(checkedAt) + 60_000;

test('operator switch-off requires complete readback of the pinned maintenance allocation', async (t) => {
  const cases = [
    { name: 'all traffic serves the pinned maintenance version' },
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
        checkedAt, now, sha: 'a'.repeat(40), workerId: 'b'.repeat(32),
        version: '12345678-1234-1234-1234-123456789abc',
        deploymentReadback(receipt) {
          const history = structuredClone(receipt);
          changeReadback?.(history, transport.env.PROMOTED_VERSION);
          return history;
        },
      });
      await transport.run('record');
      transport.shift();
      const { PROMOTED_VERSION: candidate } = transport.env;
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
