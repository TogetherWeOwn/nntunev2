import { createCipheriv, createDecipheriv, createHash, randomBytes } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { basename, join } from 'node:path';
import { isDeepStrictEqual } from 'node:util';
import { smoke } from '../../bin/smoke.mjs';
import { cloudflare, requireCompletePage } from './seed.mjs';
import { admitRecovery, intentFile, recoveryContext, workflow } from './recovery-evidence.mjs';
import proof from '../release-promotion-proof.cjs';

const name = 'nntune-web';
const deployments = `/scripts/${name}/deployments`;
const page = '?page=1&per_page=100';
const hex = (s, length) => typeof s === 'string' && s.length === length && !/[^0-9a-f]/.test(s);
const uuid = (s) => typeof s === 'string' && s.length === 36 && /^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$/.test(s);
const dnsLabel = /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/;
const validDomain = (domain) => typeof domain === 'string' && domain.length <= 253 &&
  /\.[a-z]{2,}$/.test(domain) && domain.split('.').every((label) => dnsLabel.test(label));
const allocation = (version_id, percentage = 100) => ({ version_id, percentage });
const matches = (entry, expected) => Array.isArray(entry?.versions) && entry.versions.length === expected.length &&
  expected.every((v) => entry.versions.some((actual) =>
    actual?.version_id === v.version_id && actual.percentage === v.percentage));
const seedDigest = createHash('sha256').update(readFileSync(new URL('./worker.mjs', import.meta.url))).digest('hex');

export class MaintenanceModuleMismatchError extends Error {
  constructor(recorded, observed, phase) {
    const subject = phase === 'post-write' ? 'restored maintenance module' : 'pinned maintenance module';
    const action = phase === 'post-write' ? 'Keep the writer freeze' : 'This run submitted no restore. Keep the writer freeze';
    super(`Switch-off pending: ${subject} SHA-256 ${observed} differs from the rollback receipt ${recorded}. ` +
      `${action} and reconcile the pinned maintenance version and provider audit read-only.`);
  }
}

// Only the single seeded module counts as readable maintenance content.
function moduleDigest(version) {
  const [module, ...extra] = Array.isArray(version.modules) ? version.modules : [];
  const content = module?.content_base64;
  if (extra.length || version.main_module !== 'worker.mjs' || module?.name !== 'worker.mjs' ||
      typeof content !== 'string' || !content ||
      (module.content_type !== undefined && module.content_type !== 'application/javascript+module')) return null;
  const bytes = Buffer.from(content, 'base64');
  if (bytes.toString('base64') !== content) return null;
  return createHash('sha256').update(bytes).digest('hex');
}

export function rollbackNotice(command, error) {
  if (command === 'admit-recovery' && error instanceof proof.ArtifactUnavailableError) {
    return '::error::Switch-off pending: pinned rollback intent unavailable. Read the Actions artifact metadata for the original rollout and reconcile the pinned rollback allocation and provider audit read-only. Keep the original recovery inputs and writer freeze.';
  }
  // Both digests are of reviewed public code, not protected input.
  if (error instanceof MaintenanceModuleMismatchError) return `::error::${error.message}`;
  return '::error::Protected rollback check failed. Reconcile Worker, deployment, routes and provider audit before any retry.';
}

export async function finalSmokeRollback(command, env, request = fetch, now = Date.now, probe = smoke) {
  if (!['check-seed', 'record', 'pin-final', 'restore', 'prepare-recovery', 'recover'].includes(command)) {
    throw new Error('Invalid rollback operation');
  }
  const prepare = command === 'prepare-recovery';
  const recovery = prepare || command === 'recover';
  if (recovery) recoveryContext(env);
  const { CLOUDFLARE_ACCOUNT_ID: account, CLOUDFLARE_API_TOKEN: token, WORKER_ID: worker,
    DEPLOY_SHA: sha, DOMAIN: domain, GITHUB_REPOSITORY: repository,
    GITHUB_RUN_ATTEMPT: attempt, RUNNER_TEMP: temp, INTENT_KEY: key, ROLLBACK_CUSTODY: custody } = env;
  const run = recovery ? env.RECOVERY_RUN_ID : env.GITHUB_RUN_ID;
  const context = { schema: 1, repository, workflow, job: 'deploy-production', account, worker, sha, domain, run, attempt };
  if (!hex(account, 32) || !hex(worker, 32) || !hex(sha, 40) || typeof token !== 'string' ||
      !/^[\x21-\x7e]{1,512}$/.test(token) || !hex(key, 64) || typeof temp !== 'string' || !temp ||
      typeof repository !== 'string' || !/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repository) ||
      typeof run !== 'string' || !/^[1-9][0-9]{0,19}$/.test(run) || attempt !== '1' ||
      (!recovery && (env.GITHUB_JOB !== context.job || env.GITHUB_EVENT_NAME !== 'workflow_dispatch' ||
        env.GITHUB_REF !== 'refs/heads/main' || env.GITHUB_SHA !== sha || env.GITHUB_WORKFLOW_SHA !== sha ||
        env.GITHUB_WORKFLOW_REF !== `${repository}/${workflow}@refs/heads/main`)) ||
      !validDomain(domain)) {
    throw new Error('Invalid protected production rollback context');
  }
  const proof = typeof custody === 'string' && Buffer.byteLength(custody) <= 4096 ? JSON.parse(custody) : null;
  const { maintenance, previous, references, checked_at: checkedAt } = proof ?? {};
  const target = maintenance ?? previous;
  const stamp = Date.parse(checkedAt);
  if (proof?.account !== account || proof.worker !== worker || proof.sha !== sha || proof.domain !== domain ||
      proof.private_routes_frozen !== true || proof.exclusive_writers !== true || !hex(proof.evidence_sha256, 64) ||
      !Number.isFinite(stamp) || checkedAt !== new Date(stamp).toISOString() ||
      (maintenance != null && previous != null) || !uuid(target?.version) || !uuid(target.deployment) ||
      !hex(target.sha, 40) || (maintenance ?
        !hex(maintenance.module_sha256, 64) || (!recovery && maintenance.module_sha256 !== seedDigest) :
        typeof previous?.tag !== 'string' || !previous.tag.startsWith(`${target.sha}-`) ||
        !/^[1-9][0-9]{0,15}-1$/.test(previous.tag.slice(41))) ||
      !Array.isArray(references?.domains) || references.domains.length > 1 ||
      !references.domains.every((entry) => entry?.hostname === domain) ||
      !Object.entries(references).every(([kind, entries]) => kind === 'domains' ||
        (Array.isArray(entries) && entries.length === 0))) {
    throw new Error('Missing or mismatched production rollback custody');
  }
  function requireFreshCustody() {
    const sampledAt = now();
    if (!Number.isSafeInteger(sampledAt) || sampledAt < stamp || sampledAt - stamp > 1_800_000) {
      throw new Error('Stale production rollback custody');
    }
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
  async function live(phase) {
    const { result } = await api(pinned, 'Worker identity and exposure');
    if (result.id !== worker || result.name !== name || typeof result.deployed_on !== 'string' ||
        !Number.isFinite(Date.parse(result.deployed_on)) ||
        !isDeepStrictEqual(result.references, references) || result.subdomain?.enabled !== false ||
        result.subdomain.previews_enabled !== false) throw new Error('Worker identity, routes or previews drifted');
    // Recovery cannot compare the receipt with this checkout, so it reads the module back.
    const readModule = recovery && maintenance;
    const { result: version } = await api(`${pinned}/versions/${target.version}${readModule ? '?include=modules' : ''}`,
      'Rollback target version');
    const bindings = maintenance ? [] : [{ name: 'ASSETS', type: 'assets' }];
    const digest = readModule ? moduleDigest(version) : null;
    if (version.id !== target.version || version.annotations?.['workers/tag'] !== (maintenance?.sha ?? previous.tag) ||
        !isDeepStrictEqual(version.bindings, bindings) || (readModule && !digest)) {
      throw new Error(maintenance ? 'Maintenance version drifted' : 'Previous version drifted');
    }
    if (readModule && digest !== maintenance.module_sha256) {
      throw new MaintenanceModuleMismatchError(maintenance.module_sha256, digest, phase);
    }
    return readHistory();
  }
  async function readHistory(read = api) {
    const history = await read(deployments + page, 'Deployment history');
    const list = history.result.deployments;
    requireCompletePage(history, list, list?.length, 'deployments');
    if (list.length > 100 || !list.every((row, i) => uuid(row?.id) && typeof row.created_on === 'string' &&
        Number.isFinite(Date.parse(row.created_on)) && (i === 0 || Date.parse(list[i - 1].created_on) > Date.parse(row.created_on))) ||
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
  const reserved = recovery && !prepare && env.CLAIM_VERIFIED === 'true';
  const final = recovery && !reserved ? null : unseal(finalPath);
  if ((!recovery || reserved) && (final?.nonce !== nonce ||
      (!uuid(final.deployment) && !(reserved && final.deployment === null)))) {
    throw new Error('Final allocation belongs to another rollback intent');
  }
  const isFinal = (rows) => (final?.deployment == null || rows[0]?.id === final.deployment) && rollout(rows);
  const restored = (rows) => matches(rows[0], targetOnly) && rows[0].annotations?.['workers/message'] === message &&
    isFinal(rows.slice(1));
  const alreadyVerified = `Recorded ${targetLabel} allocation already verified`;
  if (reserved) {
    function requireFreshAllocation() {
      const sampledAt = now();
      if (!Number.isSafeInteger(sampledAt) || !Number.isSafeInteger(final.checkedAt) ||
          sampledAt < final.checkedAt || sampledAt - final.checkedAt > 120_000) {
        throw new Error('Recovery allocation proof expired');
      }
    }
    requireFreshAllocation();
    const before = final.deployment === null ? await live('preflight') :
      await readHistory(cloudflare(account, token, async (url, options) => {
        try {
          return await request(url, options);
        } catch {
          requireFreshAllocation();
          return request(url, { ...options, signal: AbortSignal.timeout(15_000) });
        }
      }));
    requireFreshAllocation();
    if (restored(before) && (final.deployment === null || restored(await live('preflight')))) return alreadyVerified;
    if (final.deployment === null || !isFinal(before)) throw new Error('Stale intent or newer or foreign active deployment');
  } else {
    if (recovery) {
      const { result: version } = await api(`${pinned}/versions/${candidate}`, 'Recorded candidate version');
      if (version.id !== candidate || version.annotations?.['workers/tag'] !== tag) throw new Error('Candidate version drifted');
    }
    const before = await live('preflight');
    const complete = restored(before);
    if (!complete && !isFinal(before)) throw new Error('Stale intent or newer or foreign active deployment');
    if (prepare) return seal(finalPath, { nonce, deployment: complete ? null : before[0].id, checkedAt: now() });
    if (complete) return alreadyVerified;
    if (recovery) throw new Error('Recovery is readback only; reconcile any previous submission');
  }
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
  const after = await live('post-write');
  if (!restored(after) || (acknowledged && after[0].id !== acknowledged)) {
    throw new Error('Restore result unresolved; reconcile allocation and provider audit before any retry');
  }
  return `Recorded ${targetLabel} allocation ${acknowledged ? 'verified' : 'reconciled by readback'}`;
}

if (import.meta.main) {
  const command = process.argv[2];
  try {
    const env = { ...process.env };
    if (command === 'admit-recovery') delete process.env.INTENT_KEY; // Withhold from subprocesses.
    const verdict = command === 'admit-recovery' ? admitRecovery(env) : await finalSmokeRollback(command, env);
    // Report a failed summary write even when the allocation was verified.
    if (verdict) writeFileSync(process.stdout.fd, `${verdict}\n`);
  } catch (error) {
    // Errors may contain protected input or provider data. Keep details in private custody.
    console.error(rollbackNotice(command, error));
    process.exitCode = 1;
  }
}
