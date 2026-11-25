'use strict';

const { randomBytes } = require('node:crypto');
const { createRequire } = require('node:module');
const { resolve } = require('node:path');
const fromWeb = createRequire(resolve(__dirname, '../../apps/web/package.json'));
const fromWrangler = createRequire(fromWeb.resolve('wrangler/package.json'));
const { Miniflare, Log, LogLevel, convertV4MiniflareOptions } = fromWrangler('miniflare');

async function invoke(bundlePath, env, outboundService) {
  const bundle = resolve(bundlePath);
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
      headers: { 'x-cleanup-invocation': nonce },
      signal: AbortSignal.timeout(30_000),
    });
    const verdict = { 200: 'confirmed', 403: 'refused', 409: 'withheld', 503: 'unconfirmed' }[response.status];
    return verdict === await response.text() ? verdict : 'unconfirmed';
  } finally {
    await runtime.dispose();
  }
}

module.exports = { invoke };

if (require.main === module) {
  invoke(process.argv[2], process.env).catch(() => 'unconfirmed').then((verdict) => {
    console.log(`Private staging fixture cleanup: ${verdict}`);
    if (verdict !== 'confirmed') process.exitCode = 1;
  });
}
