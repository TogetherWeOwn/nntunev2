'use strict';

const { resolve } = require('node:path');
const loadWorker = require('../../workers/ingest/tests/fixtures/worker_fixture_wasm.cjs');

// The process runs one qualification. Keep the real Fetch transport but
// reject replies from any URL other than the signed request's exact URL.
async function qualify(bundle, endpoint, region, scope, credential) {
  const fetch = globalThis.fetch;
  globalThis.fetch = async (request) => {
    const response = await fetch(request);
    const length = response.headers.get('content-length');
    const marker = response.headers.get('x-amz-delete-marker');
    if (response.url !== request.url || response.headers.has('content-range')
        || (marker !== null && marker !== 'false')
        || (response.status === 204 && ((length !== null && length !== '0')
          || response.headers.has('transfer-encoding')))) {
      throw new Error('ambiguous provider response');
    }
    return response;
  };
  try {
    const verdict = await loadWorker(resolve(bundle)).qualify_disposable_version(
      endpoint, region, scope, credential,
    );
    if (verdict === 'qualified' || verdict === 'refused') return verdict;
  } catch {
    // Do not print exceptions, headers, input or provider bodies.
  } finally {
    globalThis.fetch = fetch;
  }
  return 'unconfirmed';
}

module.exports = { qualify };

if (require.main === module) {
  const run = process.argv.length === 4 && process.argv[2] === '--provider'
    ? qualify(process.argv[3],
      process.env.PRIVATE_PLAYBACK_STAGING_S3_ENDPOINT ?? '',
      process.env.PRIVATE_PLAYBACK_STAGING_S3_REGION ?? '',
      process.env.PRIVATE_TRIAL_PREFLIGHT_SCOPE ?? '',
      process.env.PRIVATE_TRIAL_PREFLIGHT_S3 ?? '')
    : Promise.resolve('refused');
  run.then((verdict) => {
    const proof = verdict === 'qualified'
      ? ' HEAD 200 with matching version; twice DELETE 204 and HEAD 404 with empty bodies and optional version headers absent or matching.'
      : '';
    console.log(`Private staging fixture provider preflight: ${verdict}.${proof}`);
    if (verdict !== 'qualified') process.exitCode = 1;
  });
}
