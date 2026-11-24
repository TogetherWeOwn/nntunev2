import { createCipheriv, createDecipheriv, createHash, randomBytes } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { basename, join } from 'node:path';
import { isDeepStrictEqual } from 'node:util';
import { smoke } from '../../bin/smoke.mjs';
import { cloudflare, requireCompletePage } from './seed.mjs';
import { admitRecovery, intentFile, recoveryContext, workflow } from './recovery-evidence.mjs';

const name = 'nntune-web';
const deployments = `/scripts/${name}/deployments`;
const page = '?page=1&per_page=100';
const hex = (s, length) => typeof s === 'string' && s.length === length && !/[^0-9a-f]/.test(s);
const uuid = (s) => typeof s === 'string' && s.length === 36 && /^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$/.test(s);
const allocation = (version_id, percentage = 100) => ({ version_id, percentage });
const matches = (entry, expected) => Array.isArray(entry?.versions) && entry.versions.length === expected.length &&
  expected.every((v) => entry.versions.some((actual) =>
    actual?.version_id === v.version_id && actual.percentage === v.percentage));
const seedDigest = createHash('sha256').update(readFileSync(new URL('./worker.mjs', import.meta.url))).digest('hex');

export async function finalSmokeRollback(command, env, request = fetch, now = Date.now, probe = smoke) {
  const recovery = command === 'recover';
  if (recovery) recoveryContext(env);
  const { CLOUDFLARE_ACCOUNT_ID: account, CLOUDFLARE_API_TOKEN: token, WORKER_ID: worker,
    DEPLOY_SHA: sha, DOMAIN: domain, GITHUB_REPOSITORY: repository,
    GITHUB_RUN_ATTEMPT: attempt, RUNNER_TEMP: temp, INTENT_KEY: key, ROLLBACK_CUSTODY: custody } = env;
  const run = recovery ? env.RECOVERY_RUN_ID : env.GITHUB_RUN_ID;
  const context = { schema: 1, repository, workflow, job: 'deploy-production', account, worker, sha, domain, run, attempt };
  if (!hex(account, 32) || !hex(worker, 32) || !hex(sha, 40) || !token || !hex(key, 64) || !temp ||
      !repository || !/^[1-9][0-9]{0,19}$/.test(run ?? '') || attempt !== '1' ||
      (!recovery && (env.GITHUB_JOB !== context.job || env.GITHUB_EVENT_NAME !== 'workflow_dispatch' ||
        env.GITHUB_REF !== 'refs/heads/main' || env.GITHUB_SHA !== sha || env.GITHUB_WORKFLOW_SHA !== sha ||
        env.GITHUB_WORKFLOW_REF !== `${repository}/${workflow}@refs/heads/main`)) ||
      typeof domain !== 'string' || domain.length > 253 || !/^[a-z0-9][a-z0-9.-]*\.[a-z]{2,}$/.test(domain)) {
    throw new Error('Invalid protected production rollback context');
  }
  const proof = custody?.length <= 4096 ? JSON.parse(custody) : null;
  const { maintenance, previous, references, checked_at: checkedAt } = proof ?? {};
  const target = maintenance ?? previous;
  const stamp = Date.parse(checkedAt);
  if (proof?.account !== account || proof.worker !== worker || proof.sha !== sha || proof.domain !== domain ||
      proof.private_routes_frozen !== true || proof.exclusive_writers !== true || !hex(proof.evidence_sha256, 64) ||
      !Number.isFinite(stamp) || checkedAt !== new Date(stamp).toISOString() ||
      (maintenance != null && previous != null) || !uuid(target?.version) || !uuid(target.deployment) ||
      !hex(target.sha, 40) || (maintenance ?
        !hex(maintenance.module_sha256, 64) || (!recovery && maintenance.module_sha256 !== seedDigest) :
        typeof previous.tag !== 'string' || !new RegExp(`^${target.sha}-[1-9][0-9]{0,15}-1$`).test(previous.tag)) ||
      !Array.isArray(references?.domains) || references.domains.length > 1 ||
      !references.domains.every((entry) => entry?.hostname === domain) ||
      !Object.entries(references).every(([kind, entries]) => kind === 'domains' ||
        (Array.isArray(entries) && entries.length === 0))) {
    throw new Error('Missing or mismatched production rollback custody');
  }
  function requireFreshCustody() {
    const elapsed = now() - stamp;
    if (elapsed < 0 || elapsed > 1_800_000) throw new Error('Stale production rollback custody');
  }
  const custodyDigest = createHash('sha256').update(custody).digest('hex');
  const aad = (path) => Buffer.from(JSON.stringify({ ...context, custody: custodyDigest, file: basename(path) }));
  const secret = Buffer.from(key, 'hex');
  const intentPath = join(temp, intentFile);
  const finalPath = join(temp, 'production-final-allocation.sealed');
  function seal(path, value) {
    const iv = randomBytes(12);
    const cipher = createCipheriv('aes-256-gcm', secret, iv).setAAD(aad(path));
    const ciphertext = Buffer.concat([cipher.update(JSON.stringify(value)), cipher.final()]);
    writeFileSync(path, Buffer.concat([iv, cipher.getAuthTag(), ciphertext]), { mode: 0o600, flag: 'wx' });
  }
  function unseal(path) {
    try {
      const bytes = readFileSync(path);
      const decipher = createDecipheriv('aes-256-gcm', secret, bytes.subarray(0, 12)).setAAD(aad(path));
      decipher.setAuthTag(bytes.subarray(12, 28));
      return JSON.parse(Buffer.concat([decipher.update(bytes.subarray(28)), decipher.final()]));
    } catch {
      throw new Error('Rollback intent or final allocation is missing, invalid or stale');
    }
  }
  const api = cloudflare(account, token, request);
  const pinned = `/workers/${worker}`;
  async function live() {
    const { result } = await api(pinned, 'Worker identity and exposure');
    if (result.id !== worker || result.name !== name || !Number.isFinite(Date.parse(result.deployed_on)) ||
        !isDeepStrictEqual(result.references, references) || result.subdomain?.enabled !== false ||
        result.subdomain.previews_enabled !== false) throw new Error('Worker identity, routes or previews drifted');
    const { result: version } = await api(`${pinned}/versions/${target.version}`, 'Rollback target version');
    if (version.id !== target.version || version.annotations?.['workers/tag'] !== (maintenance?.sha ?? previous.tag) ||
        !Array.isArray(version.bindings) || version.bindings.length !== 0) {
      throw new Error(maintenance ? 'Maintenance version drifted' : 'Previous version drifted');
    }
    const history = await api(deployments + page, 'Deployment history');
    const list = history.result.deployments;
    requireCompletePage(history, list, list?.length, 'deployments');
    if (list.length > 100 || !list.every((row, i) => uuid(row?.id) && Number.isFinite(Date.parse(row.created_on)) &&
        (i === 0 || Date.parse(list[i - 1].created_on) > Date.parse(row.created_on))) ||
        new Set(list.map((row) => row.id)).size !== list.length) throw new Error('Ambiguous deployment history');
    return list;
  }

  const tag = `${sha}-${run}-${attempt}`;
  const targetOnly = [allocation(target.version)];
  const isTarget = (row) => row?.id === target.deployment && matches(row, targetOnly);
  if (command === 'check-seed' || command === 'record') {
    requireFreshCustody();
    if (!isTarget((await live())[0])) throw new Error('Recorded rollback deployment is no longer active');
    if (command === 'check-seed') return;
    const inventory = await api(`${pinned}/versions${page}`, 'Candidate version inventory');
    const list = inventory.result;
    requireCompletePage(inventory, list, list?.length, 'versions');
    const candidates = list.filter((v) => v?.annotations?.['workers/tag'] === tag);
    const candidate = candidates[0]?.id;
    if (list.length > 100 || candidates.length !== 1 || !uuid(candidate) || candidate === target.version) {
      throw new Error('Candidate tag did not resolve uniquely on the pinned Worker');
    }
    if (previous && await probe(`https://${domain}`, { expectedSha: previous.sha, log: () => {} }) !== 0) {
      throw new Error('Previous build health or identity was not verified');
    }
    requireFreshCustody();
    seal(intentPath, { ...context, maintenance, previous, tag, candidate, nonce: randomBytes(16).toString('hex') });
    return;
  }

  // Freshness is checked before the rollout. The sealed intent and continuous
  // custody carry that proof through final smoke and the same-job restore.
  const intent = unseal(intentPath);
  const { candidate, nonce } = intent;
  if (!uuid(candidate) || candidate === target.version || !hex(nonce, 32) || intent.tag !== tag ||
      !isDeepStrictEqual(intent.maintenance ?? intent.previous, target)) throw new Error('Mismatched rollback intent');
  const candidateOnly = [allocation(candidate)];
  const targetLabel = maintenance ? 'maintenance' : 'previous Worker';
  const message = `${maintenance ? 'First-release maintenance' : 'Previous Worker'} recovery ${nonce}`;
  const rollout = (rows) => matches(rows[0], candidateOnly) &&
    matches(rows[1], [allocation(candidate, 50), allocation(target.version, 50)]) &&
    matches(rows[2], [allocation(candidate, 5), allocation(target.version, 95)]) && isTarget(rows[3]);
  if (command === 'pin-final') {
    const rows = await live();
    if (env.PROMOTED_VERSION !== candidate || env.PREV_VERSION !== target.version || !rollout(rows)) {
      throw new Error('Final deployment does not match the recorded rollout');
    }
    seal(finalPath, { nonce, deployment: rows[0].id });
    return;
  }
  if (command !== 'restore' && !recovery) throw new Error('Invalid rollback operation');

  // The unique candidate and complete contiguous rollout allow re-entry even
  // if promotion stopped before pin-final could record the deployment ID.
  const final = recovery ? null : unseal(finalPath);
  if (final && final.nonce !== nonce) throw new Error('Final allocation belongs to another rollback intent');
  const isFinal = (rows) => (!final || rows[0]?.id === final.deployment) && rollout(rows);
  const restored = (rows) => matches(rows[0], targetOnly) && rows[0].annotations?.['workers/message'] === message &&
    isFinal(rows.slice(1));
  const allowSubmit = !recovery || env.CLAIM_VERIFIED === 'true';
  if (recovery) {
    const { result: version } = await api(`${pinned}/versions/${candidate}`, 'Recorded candidate version');
    if (version.id !== candidate || version.annotations?.['workers/tag'] !== tag) throw new Error('Candidate version drifted');
  }
  const before = await live();
  if (restored(before)) return `Recorded ${targetLabel} allocation already verified`;
  if (!isFinal(before)) throw new Error('Stale intent or newer or foreign active deployment');
  if (!allowSubmit) throw new Error('Recovery is readback only; reconcile any previous submission');
  // The Actions lock serializes participating jobs. Custody must exclude every
  // other writer because the provider has no deployment compare-and-set.
  writeFileSync(join(temp, 'production-rollback-submitted'), '', { mode: 0o600, flag: 'wx' });
  let acknowledged;
  try {
    const { result } = await api(deployments, 'Restore recorded allocation', {
      strategy: 'percentage', versions: targetOnly, annotations: { 'workers/message': message },
    });
    if (uuid(result.id)) acknowledged = result.id;
  } catch {
    // A response can fail after committing. Always reconcile with GETs only.
  }
  const after = await live();
  if (!restored(after) || (acknowledged && after[0].id !== acknowledged)) {
    throw new Error('Restore result unresolved; reconcile allocation and provider audit before any retry');
  }
  return `Recorded ${targetLabel} allocation ${acknowledged ? 'verified' : 'reconciled by readback'}`;
}

if (import.meta.main) {
  try {
    const command = process.argv[2];
    const verdict = command === 'admit-recovery' ? admitRecovery(process.env) : await finalSmokeRollback(command, process.env);
    if (verdict) console.log(verdict);
  } catch {
    // Errors may contain protected input or provider data. Keep details in private custody.
    console.error('::error::Protected rollback check failed. Reconcile Worker, deployment, routes and provider audit before any retry.');
    process.exitCode = 1;
  }
}
