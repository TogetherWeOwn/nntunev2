'use strict';

const { createHash } = require('node:crypto');
const { resolve } = require('node:path');
const { isIP } = require('node:net');
const { connect } = require('node:tls');
const loadWorker = require('../../workers/ingest/tests/fixtures/worker_fixture_wasm.cjs');

// Fetch discards any bytes after HEAD/204 headers. Request connection close
// and read the TLS stream to EOF to verify that the wire reply ends there.
async function sendBodylessRequest(request) {
  const url = new URL(request.url);
  const hostname = url.hostname.replace(/^\[|\]$/g, '');
  const socket = connect({
    host: hostname, port: Number(url.port || 443),
    servername: isIP(hostname) ? undefined : hostname,
    ALPNProtocols: ['http/1.1'], rejectUnauthorized: true,
  });
  const abort = () => socket.destroy(new Error('provider deadline elapsed'));
  request.signal.addEventListener('abort', abort, { once: true });
  try {
    if (request.signal.aborted) throw new Error('provider deadline elapsed');
    const headers = Array.from(request.headers, ([name, value]) => `${name}: ${value}`);
    socket.write([`${request.method} ${url.pathname}${url.search} HTTP/1.1`, `Host: ${url.host}`,
      'Connection: close', 'Cache-Control: no-cache, no-store', 'Pragma: no-cache', ...headers, '', ''].join('\r\n'));
    const chunks = [];
    let size = 0;
    for await (const chunk of socket) {
      size += chunk.length;
      if (size > 32 * 1024) throw new Error('oversized provider reply');
      chunks.push(chunk);
    }
    const wire = Buffer.concat(chunks).toString('latin1');
    const end = wire.indexOf('\r\n\r\n');
    if (end < 0 || end + 4 !== wire.length) throw new Error('provider reply has invalid framing');
    const [line, ...fields] = wire.slice(0, end).split('\r\n');
    const status = /^HTTP\/1\.[01] ([2-5][0-9]{2})(?: [\x20-\x7e]*)?$/.exec(line);
    if (!status) throw new Error('invalid provider status');
    const responseHeaders = new Headers();
    for (const field of fields) {
      const colon = field.indexOf(':');
      if (colon <= 0 || /[\x00-\x08\x0a-\x1f\x7f]/.test(field)) throw new Error('invalid provider header');
      responseHeaders.append(field.slice(0, colon), field.slice(colon + 1));
    }
    const response = new Response(null, { status: Number(status[1]), headers: responseHeaders });
    Object.defineProperty(response, 'url', { value: url.href });
    return response;
  } finally {
    request.signal.removeEventListener('abort', abort);
    socket.destroy();
  }
}

// Expose the verified header-only replies to the shared WASM sender. The
// process runs a single probe and restores the original Fetch on completion.
async function qualify(bundle, endpoint, region, scope, credential, send = sendBodylessRequest) {
  const fetch = globalThis.fetch;
  globalThis.fetch = async (request) => {
    const response = await send(request);
    const length = response.headers.get('content-length');
    const marker = response.headers.get('x-amz-delete-marker');
    const transferEncoded = response.headers.has('transfer-encoding');
    if (response.url !== request.url || response.headers.has('content-range')
        || (marker !== null && marker !== 'false')
        || (length !== null && (!/^\d+$/.test(length) || transferEncoded))
        || (response.status === 204 && (transferEncoded || (length !== null && length !== '0')))) {
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

async function runProbe(args, env) {
  const [mode, bundle] = args;
  if (args.length !== 2 || (mode !== '--provider' && mode !== '--provider-dispatch')) {
    return 'refused';
  }
  const scope = env.PRIVATE_TRIAL_PREFLIGHT_SCOPE ?? '';
  if (mode === '--provider-dispatch') {
    const expected = env.PRIVATE_TRIAL_PREFLIGHT_SCOPE_SHA256 ?? '';
    if (!scope || Buffer.byteLength(scope, 'utf8') > 4096 || !/^[0-9a-f]{64}$/.test(expected)
        || createHash('sha256').update(scope, 'utf8').digest('hex') !== expected) {
      return 'refused';
    }
  }
  return qualify(bundle, env.PRIVATE_PLAYBACK_STAGING_S3_ENDPOINT ?? '',
    env.PRIVATE_PLAYBACK_STAGING_S3_REGION ?? '', scope, env.PRIVATE_TRIAL_PREFLIGHT_S3 ?? '');
}

if (require.main === module) {
  runProbe(process.argv.slice(2), process.env).then((verdict) => {
    const proof = verdict === 'qualified'
      ? ' Matched present HEAD and preserved sibling HEAD. Twice observed empty DELETE 204 and HEAD 404 with optional version headers absent or matching.'
      : '';
    console.log(`Private staging fixture provider preflight: ${verdict}.${proof}`);
    if (verdict !== 'qualified') process.exitCode = 1;
  });
}
