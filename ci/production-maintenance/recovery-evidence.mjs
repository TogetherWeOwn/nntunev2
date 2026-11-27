import { createHash, createHmac } from 'node:crypto';
import { writeFileSync, appendFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import proof from '../release-promotion-proof.cjs';

export const workflow = '.github/workflows/deploy-production.yml';
export const intentFile = 'production-rollback-intent.sealed';
const restoreStep = 'Restore recorded maintenance after failed final smoke';
const decimal = (value) => typeof value === 'string' && /^[1-9][0-9]{0,15}$/.test(value);
const hex = (value, length) => typeof value === 'string' && value.length === length && !/[^0-9a-f]/.test(value);
const terminal = ['success', 'failure', 'cancelled', 'timed_out'];

export function recoveryContext(env) {
  if (env.GITHUB_JOB !== 'recover-worker' || env.GITHUB_EVENT_NAME !== 'workflow_dispatch' ||
      env.GITHUB_REF !== 'refs/heads/main' || env.GITHUB_RUN_ATTEMPT !== '1' ||
      !hex(env.GITHUB_SHA, 40) || env.GITHUB_SHA !== env.GITHUB_WORKFLOW_SHA ||
      typeof env.GITHUB_REPOSITORY !== 'string' || !/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(env.GITHUB_REPOSITORY) ||
      env.GITHUB_WORKFLOW_REF !== `${env.GITHUB_REPOSITORY}/${workflow}@refs/heads/main` ||
      !decimal(env.GITHUB_RUN_ID) || !decimal(env.RECOVERY_RUN_ID) || env.RECOVERY_RUN_ID === env.GITHUB_RUN_ID ||
      !hex(env.DEPLOY_SHA, 40) || env.DEPLOY_SHA !== env.ACCEPTED_SHA ||
      typeof env.RUNNER_TEMP !== 'string' || !env.RUNNER_TEMP) {
    throw new Error('Invalid protected recovery dispatch');
  }
}

export function admitRecovery(env, evidence = proof.productionEvidence) {
  recoveryContext(env);
  if (!hex(env.INTENT_KEY, 64)) throw new Error('Missing recovery intent key');
  if (typeof env.GITHUB_OUTPUT !== 'string' || !env.GITHUB_OUTPUT) throw new Error('Missing recovery output path');
  const { source, artifactFor, command, list } = evidence(env.GITHUB_REPOSITORY);
  const { run, job } = source(env.RECOVERY_RUN_ID, '1');
  if (!Array.isArray(job?.steps) || !job.steps.every((entry) => typeof entry?.name === 'string' && entry.name &&
      Number.isSafeInteger(entry.number) && entry.number > 0) ||
      new Set(job.steps.map((entry) => entry.number)).size !== job.steps.length) {
    throw new Error('Unverified production recovery source');
  }
  const step = (name) => job.steps.filter((entry) => entry.name === name);
  const [intent, ...duplicates] = step('Preserve sealed rollback intent before shifting traffic');
  const [promote, ...extraPromotions] = step('Gradual promotion 5-50-100 with automatic rollback');
  const [restore, ...extraRestores] = step(restoreStep);
  if (run.status !== 'completed' || !terminal.includes(run.conclusion) || run.head_sha !== env.DEPLOY_SHA ||
      !terminal.includes(job.conclusion) || intent?.status !== 'completed' || intent.conclusion !== 'success' ||
      duplicates.length || extraPromotions.length || extraRestores.length || !promote ||
      intent.number >= promote.number || promote.status !== 'completed' || !terminal.includes(promote.conclusion) ||
      (restore && (restore.number <= promote.number || restore.status !== 'completed' ||
        ![...terminal, 'skipped'].includes(restore.conclusion)))) {
    throw new Error('Unverified production recovery source');
  }
  const { artifact, archive } = artifactFor(run, `production-rollback-intent-${env.RECOVERY_RUN_ID}-1`);
  if (`sha256:${createHash('sha256').update(archive).digest('hex')}` !== artifact.digest) {
    throw new Error('Sealed intent archive digest mismatch');
  }
  const sealed = command('python3', [fileURLToPath(new URL('../read-promotion-receipt.py', import.meta.url)),
    '--sealed-rollback-intent'], 4096, archive);
  if (!Buffer.isBuffer(sealed) || sealed.length <= 28 || sealed.length > 4096) throw new Error('Invalid sealed recovery intent');
  // An unrelated workflow must not be able to predict and reserve this name.
  const claim = createHmac('sha256', Buffer.from(env.INTENT_KEY, 'hex')).update(`claim:${env.RECOVERY_RUN_ID}`).digest('hex');
  const name = `production-worker-recovery-claim-${claim}`;
  const sourceDidNotRestore = !restore || restore.conclusion === 'skipped';
  const allowSubmit = sourceDidNotRestore && list(`artifacts?name=${name}`, 'artifacts').length === 0;
  writeFileSync(join(env.RUNNER_TEMP, intentFile), sealed, { mode: 0o600, flag: 'wx' });
  appendFileSync(env.GITHUB_OUTPUT, `allow-submit=${allowSubmit}\nclaim-name=${name}\n`);
}
