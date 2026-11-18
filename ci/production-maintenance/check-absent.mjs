export function requireAbsent(response, worker) {
  if (!worker || response?.success !== true || !Array.isArray(response.errors) ||
      response.errors.length !== 0 || !Array.isArray(response.result) ||
      response.result.some((row) => typeof row?.id !== 'string' || !row.id) ||
      response.result_info != null) {
    throw new Error('Cannot establish production Worker absence from this inventory');
  }
  if (response.result.some(({ id }) => id === worker)) {
    throw new Error('Production Worker already exists; bootstrap never overwrites it');
  }
}

if (import.meta.main) {
  const { CLOUDFLARE_ACCOUNT_ID: account, CLOUDFLARE_API_TOKEN: token, WORKER_NAME: worker } = process.env;
  if (!/^[0-9a-f]{32}$/.test(account ?? '') || !token) {
    throw new Error('Production Cloudflare account or token missing or malformed');
  }
  const inventory = await fetch(`https://api.cloudflare.com/client/v4/accounts/${account}/workers/scripts`, {
    headers: { Authorization: `Bearer ${token}`, Accept: 'application/json' },
    redirect: 'error',
    signal: AbortSignal.timeout(15_000),
  });
  if (!inventory.ok) throw new Error(`Worker inventory failed: HTTP ${inventory.status}`);
  requireAbsent(await inventory.json(), worker);
}
