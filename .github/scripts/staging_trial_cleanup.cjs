'use strict';

const { randomBytes } = require('node:crypto');
const { createRequire } = require('node:module');
const { resolve } = require('node:path');

async function cleanupVerdict(response) {
  const verdict = { 200: 'confirmed', 403: 'refused', 409: 'withheld', 503: 'unconfirmed' }[response.status];
  if (!verdict || !response.body || response.redirected) return 'unconfirmed';
  const expected = Buffer.from(verdict);
  const reader = response.body.getReader();
  let offset = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) return offset === expected.length ? verdict : 'unconfirmed';
      const end = offset + value.length;
      if (end > expected.length || !expected.subarray(offset, end).equals(value)) return 'unconfirmed';
      offset = end;
    }
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}

async function invoke(bundlePath, env, outboundService) {
  const bundle = resolve(bundlePath);
  const fromWeb = createRequire(resolve(__dirname, '../../apps/web/package.json'));
  const fromWrangler = createRequire(fromWeb.resolve('wrangler/package.json'));
  const { Miniflare, Log, LogLevel, convertV4MiniflareOptions } = fromWrangler('miniflare');
  const bindings = Object.fromEntries([
    'CLEANUP_STAGE', 'CLEANUP_SCOPE', 'CLEANUP_SCOPE_SHA256', 'CLEANUP_APPROVAL',
    'CLEANUP_S3', 'CLEANUP_DATABASE_URL',
  ].map((name) => [name, env[name] ?? '']));
  const nonce = randomBytes(32).toString('hex');
  const options = convertV4MiniflareOptions({
    host: '127.0.0.1',
    cf: false,
    log: new Log(LogLevel.NONE),
    compatibilityDate: '2026-10-01',
    modulesRoot: bundle,
    modules: [
      { type: 'ESModule', path: resolve(bundle, 'worker/shim.mjs') },
      { type: 'ESModule', path: resolve(bundle, 'index.js') },
      { type: 'CompiledWasm', path: resolve(bundle, 'index_bg.wasm') },
    ],
    bindings: { ...bindings, CLEANUP_INVOCATION: nonce },
    outboundService,
  });
  const runtime = new Miniflare({ ...options, telemetry: { enabled: false } });
  try {
    const response = await runtime.dispatchFetch('https://cleanup.invalid/', {
      method: 'POST',
      redirect: 'error',
      headers: { 'x-cleanup-invocation': nonce },
      signal: AbortSignal.timeout(30_000),
    });
    return await cleanupVerdict(response);
  } finally {
    await runtime.dispose();
  }
}

module.exports = { cleanupVerdict, invoke };

if (require.main === module) {
  const result = process.argv.length === 3 ? invoke(process.argv[2], process.env) : Promise.resolve('unconfirmed');
  result.catch(() => 'unconfirmed').then((verdict) => {
    console.log(`Private staging fixture cleanup: ${verdict}`);
    if (verdict !== 'confirmed') process.exitCode = 1;
  });
}
