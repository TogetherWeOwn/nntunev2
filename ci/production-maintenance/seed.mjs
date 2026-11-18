import { randomUUID } from 'node:crypto';
import { appendFileSync, readFileSync } from 'node:fs';

const workerId = /^[0-9a-f]{32}$/;
const versionId = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const subdomain = { enabled: false, previews_enabled: false };

function isUnbound(result) {
  return result?.subdomain?.enabled === false && result.subdomain.previews_enabled === false;
}

export async function seedMaintenance({ account, token, worker, request = fetch }) {
  if (!workerId.test(account ?? '') || !token || worker !== 'nntune-web') {
    throw new Error('Missing or invalid production account, token or Worker name');
  }
  const name = `nntune-maint-${randomUUID().replaceAll('-', '')}`;
  const code = readFileSync(new URL('./worker.mjs', import.meta.url));
  async function write(method, path, data) {
    const response = await request(`https://api.cloudflare.com/client/v4/accounts/${account}/workers/workers${path}`, {
      method, body: JSON.stringify(data), redirect: 'error', signal: AbortSignal.timeout(15_000),
      headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
    });
    if (!response.ok) throw new Error(`Maintenance ${method} refused: HTTP ${response.status}`);
    const { success, errors, result } = await response.json();
    if (success !== true || !Array.isArray(errors) || errors.length !== 0 || !result) {
      throw new Error(`Maintenance ${method} returned no confirmed result`);
    }
    return result;
  }

  // Both create and rename reject name conflicts. Only the freshly created
  // immutable ID is ever written; nntune-web is never used as a write target.
  console.log(`Creating unbound maintenance Worker ${name}`);
  const created = await write('POST', '', { name, subdomain });
  if (!workerId.test(created.id) || created.name !== name || !isUnbound(created) || created.deployed_on !== null) {
    throw new Error('No confirmed new, undeployed maintenance Worker');
  }
  const path = `/${created.id}`;
  console.log(`Maintenance Worker ID ${created.id}; reconcile this ID before any retry`);
  const version = await write('POST', `${path}/versions?deploy=true`, {
    main_module: 'worker.mjs', compatibility_date: '2026-10-01',
    modules: [{ name: 'worker.mjs', content_type: 'application/javascript+module', content_base64: code.toString('base64') }],
  });
  if (!versionId.test(version.id)) throw new Error('Maintenance version ID missing or malformed');
  const renamed = await write('PATCH', path, { name: worker, subdomain });
  if (renamed.id !== created.id || renamed.name !== worker || !isUnbound(renamed)) {
    throw new Error('No confirmed rename of the unbound maintenance Worker');
  }
  return version.id;
}

if (import.meta.main) {
  const { CLOUDFLARE_ACCOUNT_ID: account, CLOUDFLARE_API_TOKEN: token, WORKER_NAME: worker } = process.env;
  const version = await seedMaintenance({ account, token, worker });
  appendFileSync(process.env.GITHUB_OUTPUT, `version=${version}\n`);
}
