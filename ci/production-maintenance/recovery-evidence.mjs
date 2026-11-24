import { createHash, createHmac } from 'node:crypto';
import { writeFileSync, appendFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import proof from '../release-promotion-proof.cjs';

export const workflow = '.github/workflows/deploy-production.yml';
export const intentFile = 'production-rollback-intent.sealed';
const restoreStep = 'Restore recorded maintenance after failed final smoke';
const decimal = (value) => typeof value === 'string' && /^[1-9][0-9]{0,15}$/.test(value);
const terminal = ['success', 'failure', 'cancelled', 'timed_out'];

export function recoveryContext(env) {
  if (env.GITHUB_JOB !== 'recover-worker' || env.GITHUB_EVENT_NAME !== 'workflow_dispatch' ||
      env.GITHUB_REF !== 'refs/heads/main' || env.GITHUB_RUN_ATTEMPT !== '1' ||
      !/^[0-9a-f]{40}$/.test(env.GITHUB_SHA ?? '') || env.GITHUB_SHA !== env.GITHUB_WORKFLOW_SHA ||
      env.GITHUB_WORKFLOW_REF !== `${env.GITHUB_REPOSITORY}/${workflow}@refs/heads/main` ||
      !decimal(env.GITHUB_RUN_ID) || !decimal(env.RECOVERY_RUN_ID) || env.RECOVERY_RUN_ID === env.GITHUB_RUN_ID ||
      !/^[0-9a-f]{40}$/.test(env.DEPLOY_SHA ?? '') || env.DEPLOY_SHA !== env.ACCEPTED_SHA || !env.RUNNER_TEMP) {
    throw new Error('Invalid protected recovery dispatch');
  }
}

export function admitRecovery(env, evidence = proof.productionEvidence) {
  recoveryContext(env);
  const { source, artifactFor, command, list } = evidence(env.GITHUB_REPOSITORY);
  const { run, job } = source(env.RECOVERY_RUN_ID, '1');
  const step = (name) => job.steps.filter((entry) => entry.name === name);
  const [intent, ...duplicates] = step('Preserve sealed rollback intent before shifting traffic');
  const [promote, ...extraPromotions] = step('Gradual promotion 5-50-100 with automatic rollback');
  const restores = step(restoreStep);
  if (run.status !== 'completed' || !terminal.includes(run.conclusion) || run.head_sha !== env.DEPLOY_SHA ||
      !terminal.includes(job.conclusion) || intent?.status !== 'completed' || intent.conclusion !== 'success' ||
      duplicates.length || extraPromotions.length || restores.length > 1 ||
      !Number.isSafeInteger(intent.number) || !Number.isSafeInteger(promote?.number) ||
      intent.number >= promote.number || promote.status !== 'completed' || !terminal.includes(promote.conclusion)) {
    throw new Error('Unverified production recovery source');
  }
  const { artifact, archive } = artifactFor(run, `production-rollback-intent-${env.RECOVERY_RUN_ID}-1`);
  if (`sha256:${createHash('sha256').update(archive).digest('hex')}` !== artifact.digest) {
    throw new Error('Sealed intent archive digest mismatch');
  }
  const sealed = command('python3', [fileURLToPath(new URL('../read-promotion-receipt.py', import.meta.url)),
    '--sealed-rollback-intent'], 4096, archive);
  if (!/^[0-9a-f]{64}$/.test(env.INTENT_KEY ?? '')) throw new Error('Missing recovery intent key');
  // An unrelated workflow must not be able to predict and reserve this name.
  const claim = createHmac('sha256', Buffer.from(env.INTENT_KEY, 'hex')).update(`claim:${env.RECOVERY_RUN_ID}`).digest('hex');
  const name = `production-worker-recovery-claim-${claim}`;
  const sourceDidNotRestore = !restores.length || restores[0].conclusion === 'skipped';
  const allowSubmit = sourceDidNotRestore && list(`artifacts?name=${name}`, 'artifacts').length === 0;
  writeFileSync(join(env.RUNNER_TEMP, intentFile), sealed, { mode: 0o600, flag: 'wx' });
  appendFileSync(env.GITHUB_OUTPUT, `allow-submit=${allowSubmit}\nclaim-name=${name}\n`);
}
