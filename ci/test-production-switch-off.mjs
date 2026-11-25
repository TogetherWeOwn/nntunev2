import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import { finalSmokeFixture, recoveryFixture } from './fixtures/production-worker-transport.mjs';
import { intentFile, workflow } from './production-maintenance/recovery-evidence.mjs';

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

test('operator switch-off resumes read-only after incomplete deployment history', async (t) => {
  let incompleteHistory = true;
  const transport = finalSmokeFixture(t, {
    ...fixture,
    deploymentReadback(receipt) {
      if (incompleteHistory) receipt.result_info.total_count += 1;
      return receipt;
    },
  });
  await transport.run('record');
  transport.shift();
  const rollout = structuredClone(transport.rows);
  const { enter, fresh } = recoveryFixture(t, transport, 'success');
  const incomplete = /deployments on a complete first page/;
  await assert.rejects(enter(fresh('13')), incomplete);
  const { calls, rows } = transport;
  assert.deepEqual(calls.slice(calls.indexOf('POST')), ['POST', 'GET', 'GET', 'GET']);
  assert.deepEqual(rows[0].versions, [{ version_id: version, percentage: 100 }]);
  assert.deepEqual(rows.slice(1), rollout);
  const restoredHistory = structuredClone(rows);

  const pending = fresh('14');
  let before = calls.length;
  await assert.rejects(enter(pending), incomplete);
  assert.match(readFileSync(pending.GITHUB_OUTPUT, 'utf8'), /^allow-submit=false$/m);
  assert.deepEqual(calls.slice(before), ['GET', 'GET', 'GET', 'GET']);
  assert.deepEqual(rows, restoredHistory);

  incompleteHistory = false;
  const resume = fresh('15');
  before = calls.length;
  assert.equal(await enter(resume), 'Recorded maintenance allocation already verified');
  assert.match(readFileSync(resume.GITHUB_OUTPUT, 'utf8'), /^allow-submit=false$/m);
  assert.deepEqual(calls.slice(before), ['GET', 'GET', 'GET', 'GET']);
  assert.deepEqual(rows, restoredHistory);
});

test('operator recovers the existing rollback run after the dispatch response is lost', () => {
  const source = readFileSync(new URL(`../${workflow}`, import.meta.url), 'utf8');
  const job = source.split('\n  preflight:\n')[1]?.split(/\n {2}\S/)[0];
  assert.match(job, /^    name: production-preflight$/m);
  const target = job.match(/^      - name: Resolve and validate the promotion target\n        id: target\n        env:\n((?: {10}\S[^\n]*\n)+)        run: \|$/m);
  assert.ok(target, 'target resolution step must expose the dispatch inputs in its environment');
  const envLines = target[1].split('\n');
  for (const [key, input] of Object.entries({ DEPLOY_MODE: 'mode', INPUT_SHA: 'sha', RECOVERY_RUN_ID: 'recovery_run_id' })) {
    assert.ok(envLines.includes(`          ${key}: \${{ inputs.${input} }}`), `${key} must expose the submitted ${input}`);
  }

  const repository = { id: 114, full_name: 'example/fixture' };
  const workflowId = 77;
  const run = {
    id: 102, run_attempt: 1, workflow_id: workflowId, path: workflow, event: 'workflow_dispatch',
    head_branch: 'main', repository, head_repository: repository,
    head_sha: 'c'.repeat(40), created_at: '2026-10-01T12:10:01Z', status: 'in_progress',
  };
  const inputs = { DEPLOY_MODE: 'recover-worker', INPUT_SHA: fixture.sha, RECOVERY_RUN_ID: '101' };
  const runs = [];
  let dispatches = 0;
  const start = () => {
    dispatches += 1;
    runs.push(run);
    throw new Error('synthetic response lost after the run started');
  };
  assert.throws(start, /response lost after the run started/);

  // Model the handoff's read-only decision from run metadata and the inputs
  // observed in the target-resolution step of the same preflight attempt.
  const pending = { start: 'pending readback', switchOff: 'pending' };
  const confirmStart = ({ total_count, workflow_runs }, preflight) => {
    if (total_count !== 1 || workflow_runs.length !== 1) return pending;
    const [seen] = workflow_runs;
    if (!Number.isSafeInteger(seen.id) || seen.id <= 0 || seen.run_attempt !== 1 ||
        seen.repository?.id !== repository.id || seen.head_repository?.id !== repository.id ||
        seen.workflow_id !== workflowId || seen.path !== workflow ||
        seen.event !== 'workflow_dispatch' || seen.head_branch !== 'main' ||
        !(seen.created_at >= '2026-10-01T12:10:00Z' && seen.created_at <= '2026-10-01T12:10:10Z') ||
        preflight?.run_id !== seen.id || preflight.run_attempt !== seen.run_attempt ||
        preflight.inputs?.DEPLOY_MODE !== 'recover-worker' || preflight.inputs?.INPUT_SHA !== fixture.sha ||
        preflight.inputs?.RECOVERY_RUN_ID !== '101') return pending;
    return { ...pending, start: 'confirmed', runId: seen.id, attempt: seen.run_attempt };
  };

  const listing = { total_count: 1, workflow_runs: runs };
  const preflight = { run_id: 102, run_attempt: 1, inputs };
  assert.equal(confirmStart({ total_count: 0, workflow_runs: [] }, null), pending);
  assert.equal(confirmStart({ ...listing, total_count: 2 }, preflight), pending);
  assert.equal(confirmStart({ total_count: 2, workflow_runs: [run, { ...run, id: 103 }] }, preflight), pending);
  assert.equal(confirmStart(listing, null), pending);
  assert.equal(confirmStart(listing, { ...preflight, run_id: 103 }), pending);
  assert.equal(confirmStart(listing, { ...preflight, run_attempt: 2 }), pending);
  assert.equal(confirmStart(listing, { ...preflight, inputs: { ...inputs, DEPLOY_MODE: 'promote' } }), pending);
  assert.equal(confirmStart(listing, { ...preflight, inputs: { ...inputs, INPUT_SHA: run.head_sha } }), pending);
  assert.equal(confirmStart(listing, { ...preflight, inputs: { ...inputs, RECOVERY_RUN_ID: '100' } }), pending);
  assert.deepEqual(confirmStart(listing, preflight), {
    start: 'confirmed', runId: 102, attempt: 1, switchOff: 'pending',
  });
  assert.deepEqual(runs, [run]);
  assert.equal(dispatches, 1);
});
