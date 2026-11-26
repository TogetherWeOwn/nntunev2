import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import { spawnSync } from 'node:child_process';
import { open, readFile, writeFile } from 'node:fs/promises';
import { finalSmokeFixture, recoveryFixture } from './fixtures/production-worker-transport.mjs';
import { intentFile, workflow } from './production-maintenance/recovery-evidence.mjs';

const checkedAt = '2026-10-01T12:00:00Z';
const now = () => Date.parse(checkedAt) + 60_000;
const version = '12345678-1234-1234-1234-123456789abc';
const fixture = { checkedAt, now, sha: 'a'.repeat(40), workerId: 'b'.repeat(32), version };

test('rollback retries read-only after the verified summary cannot be written', async (t) => {
  const { calls, custody, env, prepare, request, rows, run, temp } = finalSmokeFixture(t, fixture);
  await prepare();
  assert.equal(await run('restore'), 'Recorded maintenance allocation verified');
  assert.deepEqual(rows[0].versions, [{ version_id: version, percentage: 100 }]);
  assert.equal(calls.filter((method) => method === 'POST').length, 1);

  const checkpoint = [intentFile, 'production-final-allocation.sealed', 'production-rollback-submitted'];
  const readCheckpoint = () => Promise.all(checkpoint.map((name) => readFile(join(temp, name))));
  const before = await readCheckpoint();
  const prefix = `https://api.cloudflare.com/client/v4/accounts/${env.CLOUDFLARE_ACCOUNT_ID}/workers`;
  const worker = `${prefix}/workers/${fixture.workerId}`;
  const urls = [worker, `${worker}/versions/${version}`,
    `${prefix}/scripts/nntune-web/deployments?page=1&per_page=100`];
  const replies = [];
  for (const url of urls) {
    const response = await request(url, { method: 'GET', redirect: 'error', headers: { Authorization: 'Bearer fixture-only' } });
    replies.push([url, await response.json()]);
  }
  const preload = `
    import assert from 'node:assert/strict';
    const replies = new Map(${JSON.stringify(replies)});
    globalThis.fetch = async (url, options) => {
      assert.equal(options.method, 'GET');
      assert.ok(replies.has(url));
      const response = Response.json(replies.get(url));
      replies.delete(url);
      return response;
    };
    process.on('exit', () => assert.equal(replies.size, 0, 'Expected the complete serving readback'));
  `;
  const summary = join(temp, 'recovery-summary');
  await writeFile(summary, '');
  async function restoreTo(flags) {
    const output = await open(summary, flags);
    try {
      return spawnSync(process.execPath, ['--import', `data:text/javascript,${encodeURIComponent(preload)}`,
        'ci/production-maintenance/final-smoke-rollback.mjs', 'restore'], {
        env: { ...env, ROLLBACK_CUSTODY: JSON.stringify(custody) },
        stdio: ['ignore', output.fd, 'pipe'], encoding: 'utf8', timeout: 5_000,
      });
    } finally {
      await output.close();
    }
  }

  const failed = await restoreTo('r');
  assert.equal(failed.status, 1, failed.stderr);
  assert.equal(failed.stderr, '::error::Protected rollback check failed. Reconcile Worker, deployment, routes and provider audit before any retry.\n');
  assert.equal(await readFile(summary, 'utf8'), '');
  assert.deepEqual(await readCheckpoint(), before);

  const retry = await restoreTo('w');
  assert.equal(retry.status, 0, retry.stderr);
  assert.equal(retry.stderr, '');
  assert.equal(await readFile(summary, 'utf8'), 'Recorded maintenance allocation already verified\n');
  assert.deepEqual(await readCheckpoint(), before);
});

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

test('operator recovers the existing rollback run after the dispatch response is lost', async (t) => {
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
  const recorded = confirmStart(listing, preflight);
  assert.deepEqual(recorded, {
    start: 'confirmed', runId: 102, attempt: 1, switchOff: 'pending',
  });

  const resume = (record, read) => {
    const path = `/repos/${repository.full_name}/actions/runs/${record.runId}/attempts/${record.attempt}`;
    const request = { method: 'GET', path };
    const { nextRead, nextAction, ...confirmed } = record;
    const pending = { ...confirmed, status: 'pending', nextRead: request };
    let reply;
    try {
      reply = read(request);
    } catch {
      return pending;
    }
    if (reply?.id !== record.runId || reply.run_attempt !== record.attempt) return pending;
    const observed = { ...confirmed, status: reply.status, conclusion: reply.conclusion };
    if (reply.status !== 'waiting' && reply.status !== 'in_progress') return observed;

    const jobsRequest = { method: 'GET', path: `${path}/jobs` };
    const checkJob = { ...observed, nextRead: jobsRequest };
    let listing;
    try {
      listing = read(jobsRequest);
    } catch {
      return checkJob;
    }
    if (!Array.isArray(listing?.jobs) || listing.total_count !== listing.jobs.length) return checkJob;
    const jobs = listing.jobs.filter((job) => job.name === 'recover-worker');
    const [job] = jobs;
    if (jobs.length !== 1 || job.run_id !== record.runId || job.run_attempt !== record.attempt) return checkJob;
    if (job.status === 'waiting') return { ...checkJob, nextAction: 'check-production-environment-protection' };
    if (job.status === 'in_progress') return { ...checkJob, nextAction: 'read-recovery-job' };
    return checkJob;
  };

  const nextRead = { method: 'GET', path: '/repos/example/fixture/actions/runs/102/attempts/1' };
  await t.test('a temporary status failure retries the recorded attempt read-only', () => {
    const reads = [];
    const readRun = (request) => {
      reads.push(request);
      if (reads.length === 1) throw new Error('synthetic status read unavailable');
      return { ...run, status: 'completed', conclusion: 'success' };
    };
    const unavailable = resume(recorded, readRun);
    assert.deepEqual(unavailable, { ...recorded, status: 'pending', nextRead });
    assert.deepEqual(reads, [nextRead]);
    assert.equal(dispatches, 1);

    assert.deepEqual(resume(unavailable, readRun), { ...recorded, status: 'completed', conclusion: 'success' });
    assert.deepEqual(reads, [nextRead, nextRead]);
  });

  await t.test('the recorded rollback waits for production and resumes verification on the same attempt', () => {
    const recoveryJob = source.split('\n  recover-worker:\n')[1]?.split(/\n {2}\S/)[0];
    assert.match(recoveryJob, /^    name: recover-worker$/m);
    assert.match(recoveryJob, /^    environment: production$/m);

    const reads = [];
    const nextJobRead = { method: 'GET', path: '/repos/example/fixture/actions/runs/102/attempts/1/jobs' };
    let status = 'waiting';
    let jobAttempt = 1;
    const read = (request) => {
      reads.push(request);
      if (request.path === nextRead.path) return { ...run, status, conclusion: null };
      assert.deepEqual(request, nextJobRead);
      return { total_count: 1, jobs: [{ id: 103, name: 'recover-worker', run_id: 102,
        run_attempt: jobAttempt, status, conclusion: null }] };
    };

    const waiting = resume(recorded, read);
    assert.deepEqual(waiting, { ...recorded, status: 'waiting', conclusion: null,
      nextRead: nextJobRead, nextAction: 'check-production-environment-protection' });

    status = 'in_progress';
    jobAttempt = 2;
    const unconfirmed = resume(waiting, read);
    assert.deepEqual(unconfirmed, { ...recorded, status: 'in_progress', conclusion: null, nextRead: nextJobRead });

    jobAttempt = 1;
    assert.deepEqual(resume(unconfirmed, read), { ...recorded, status: 'in_progress', conclusion: null,
      nextRead: nextJobRead, nextAction: 'read-recovery-job' });
    assert.deepEqual(reads, [nextRead, nextJobRead, nextRead, nextJobRead, nextRead, nextJobRead]);
  });

  assert.deepEqual(runs, [run]);
  assert.equal(dispatches, 1);
});
