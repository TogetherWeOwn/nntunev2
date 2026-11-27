import { randomUUID } from 'node:crypto';
import { readFileSync } from 'node:fs';

const name = 'nntune-web';
const hexId = /^[0-9a-f]{32}$/;
const shaPattern = /^[0-9a-f]{40}$/;
const versionId = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const timestamp = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/;
const matches = (pattern, value) => typeof value === 'string' && pattern.test(value);
const empty = (value) => Array.isArray(value) && value.length === 0;
const page = '?page=1&per_page=2';

function requireRouteProof(raw, worker, sha, now) {
  let proof;
  if (typeof raw !== 'string') throw new Error('Missing or malformed custodian route proof');
  try {
    proof = JSON.parse(raw);
  } catch {
    throw new Error('Missing or malformed custodian route proof');
  }
  const stamp = proof?.checked_at;
  const checkedAt = typeof stamp === 'string' ? Date.parse(stamp) : NaN;
  if (proof?.worker_id !== worker || proof.sha !== sha || proof.route_count !== 0 ||
      !Number.isFinite(checkedAt) || stamp !== new Date(checkedAt).toISOString().slice(0, 19) + 'Z' ||
      !Number.isFinite(now) || checkedAt > now || now - checkedAt > 30 * 60 * 1000) {
    throw new Error('Custodian route proof must be fresh, empty and bound to this Worker and SHA');
  }
}

export function requireCompletePage(response, items, count, label) {
  const info = response.result_info;
  const pages = info?.total_pages;
  if (!Array.isArray(items) || items.length !== count || info?.page !== 1 || info.count !== count ||
      info.total_count !== count || (pages !== undefined && pages !== 1 && !(count === 0 && pages === 0))) {
    throw new Error(`Expected exactly ${count} ${label} on a complete first page`);
  }
}

export function cloudflare(account, token, request) {
  const root = `https://api.cloudflare.com/client/v4/accounts/${account}/workers`;
  return async function api(path, label, data) {
    let response;
    let body;
    try {
      response = await request(root + path, {
        method: data ? 'POST' : 'GET', redirect: 'error', signal: AbortSignal.timeout(15_000),
        headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
        body: data ? JSON.stringify(data) : undefined,
      });
      if (response.ok) body = await response.json();
    } catch {
      throw new Error(`${label}: Cloudflare request or JSON read failed`);
    }
    if (!response.ok) throw new Error(`${label}: Cloudflare refused (HTTP ${response.status})`);
    if (body?.success !== true || !empty(body.errors) || body.result == null) {
      throw new Error(`${label}: Cloudflare returned no confirmed result`);
    }
    return body;
  };
}

export async function seedMaintenance({ account, token, worker, sha, routeProof, request = fetch, now = Date.now }) {
  if (!matches(hexId, account) || !matches(hexId, worker) || !matches(shaPattern, sha) ||
      !matches(/^[A-Za-z0-9._~+/-]+=*$/, token)) {
    throw new Error('Missing or invalid production account, immutable Worker ID, token or SHA');
  }
  const pinned = `/workers/${worker}`;
  const script = `/scripts/${name}`;
  const api = cloudflare(account, token, request);

  async function workerState(deployed) {
    const { result } = await api(pinned, 'Worker identity');
    const deployedOn = result.deployed_on;
    const hasExpectedState = deployed
      ? matches(timestamp, deployedOn) && Number.isFinite(Date.parse(deployedOn))
      : deployedOn === null;
    if (result.id !== worker || result.name !== name ||
        !empty(result.references?.domains) || !Object.values(result.references).every(empty) ||
        result.subdomain?.enabled !== false || result.subdomain.previews_enabled !== false ||
        !hasExpectedState) {
      throw new Error(`Pinned Worker is not confirmed ${deployed ? 'deployed and' : 'undeployed and'} unexposed`);
    }
  }

  requireRouteProof(routeProof, worker, sha, now());
  await workerState(false);
  // Cloudflare exposes these deployment and settings reads by name only.
  const deployments = await api(`${script}/deployments${page}`, 'Deployment inventory');
  requireCompletePage(deployments, deployments.result.deployments, 0, 'deployments');
  const versions = await api(`${pinned}/versions${page}`, 'Version inventory');
  requireCompletePage(versions, versions.result, 0, 'versions');
  const { result: settings } = await api(`${script}/settings`, 'Binding inventory');
  if (!empty(settings.bindings)) throw new Error('Worker bindings are not confirmed empty');

  const code = readFileSync(new URL('./worker.mjs', import.meta.url));
  requireRouteProof(routeProof, worker, sha, now());
  const marker = `Maintenance seed ${randomUUID()}`;
  // No provider-side compare-and-set: the protected lane must be the sole
  // Worker writer from the first GET through readback. Never retry the POST.
  const submission = await api(`${pinned}/versions?deploy=true`, 'Seed upload and deploy', {
    main_module: 'worker.mjs', compatibility_date: '2026-10-01', bindings: [],
    annotations: { 'workers/tag': sha, 'workers/message': marker },
    modules: [{ name: 'worker.mjs', content_type: 'application/javascript+module', content_base64: code.toString('base64') }],
  }).catch(() => null); // The upload may have committed before the response failed.

  const active = await api(`${script}/deployments${page}`, 'Deployment readback');
  requireCompletePage(active, active.result.deployments, 1, 'deployments');
  const deployment = active.result.deployments[0];
  const deployed = deployment?.versions;
  const id = deployed?.[0]?.version_id;
  if (typeof deployment?.id !== 'string' || !versionId.test(deployment.id) ||
      !Array.isArray(deployed) || deployed.length !== 1 ||
      typeof id !== 'string' || !versionId.test(id) || deployed[0].percentage !== 100 ||
      (submission && submission.result.id !== id)) {
    throw new Error('Readback did not confirm exactly the new seed version at 100%');
  }
  const inventory = await api(`${pinned}/versions${page}`, 'Version inventory readback');
  requireCompletePage(inventory, inventory.result, 1, 'versions');
  const { result: version } = await api(`${pinned}/versions/${id}`, 'Seed version readback');
  if (inventory.result[0]?.id !== id || version.id !== id || version.annotations?.['workers/tag'] !== sha ||
      version.annotations['workers/message'] !== marker || !empty(version.bindings)) {
    throw new Error('Readback did not confirm this submission as the only binding-free seed');
  }
  await workerState(true);
  return id;
}

if (import.meta.main) {
  const { CLOUDFLARE_ACCOUNT_ID: account, CLOUDFLARE_API_TOKEN: token, WORKER_ID: worker,
    DEPLOY_SHA: sha, ROUTE_PROOF: routeProof } = process.env;
  const undo = 'Close the production enable window and verify the seed remains unrouted and unexposed; do not delete the seed. See docs/ops.md.';
  try {
    const version = await seedMaintenance({ account, token, worker, sha, routeProof });
    console.log(`Maintenance seed ${version} verified at 100% on the pinned Worker. Undo: ${undo}`);
  } catch (error) {
    console.error(`::error::${error.message}. Reconcile Worker, deployment and route state before any retry. ${undo}`);
    process.exitCode = 1;
  }
}
