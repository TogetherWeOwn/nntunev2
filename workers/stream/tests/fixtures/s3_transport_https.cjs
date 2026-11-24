// Loopback S3-shaped origin for the unmounted Wasm playback transport.
const assert = require('node:assert/strict');
const { createHash, createHmac } = require('node:crypto');
const { once } = require('node:events');
const { readFileSync } = require('node:fs');
const { createServer } = require('node:https');
const { registerHooks } = require('node:module');
const { test } = require('node:test');
const { pathToFileURL } = require('node:url');

// The production bundle also imports the existing NNTP socket adapter.
// Make that import fail on use while keeping real Fetch and loopback TLS.
const sockets = pathToFileURL(require.resolve('./s3_fixture_sockets.cjs')).href;
const hook = registerHooks({
  resolve(specifier, context, nextResolve) {
    if (specifier === 'cloudflare:sockets') return { url: sockets, shortCircuit: true };
    return nextResolve(specifier, context);
  },
});
const { make_s3_fixture } = require(process.env.S3_FIXTURE_WASM);
hook.deregister();
const path = '/fixture-media/media/00000000000040008000000000000003/00000000000040008000000000000004';
const query = 'versionId=version%2B%2F%25%3F%3D%26';
const span = 'bytes 65600-65616/65617';
const hash = (value) => createHash('sha256').update(value).digest('hex');
const mac = (key, value) => createHmac('sha256', key).update(value).digest();

function verifyGet(req, host) {
  assert.equal(req.method, 'GET');
  assert.equal(req.url, `${path}?${query}`);
  assert.equal(req.headers.host, host);
  assert.equal(req.headers.range, 'bytes=65600-65616');
  // Undici also appends identity automatically for a ranged request.
  assert.match(req.headers['accept-encoding'], /^identity(?:, identity)?$/);
  assert.equal(req.headers['x-amz-content-sha256'], hash(''));
  const date = req.headers['x-amz-date'];
  assert.match(date, /^\d{8}T\d{6}Z$/);
  const signed = 'host;range;x-amz-content-sha256;x-amz-date';
  const headers = signed.split(';').map(key => `${key}:${req.headers[key]}\n`).join('');
  const canonical = ['GET', path, query, headers, signed, hash('')].join('\n');
  const scope = `${date.slice(0, 8)}/fixture-1/s3/aws4_request`;
  const key = scope.split('/').reduce(mac, Buffer.from('AWS4fixture-reader-signing-material'));
  const signature = mac(key, ['AWS4-HMAC-SHA256', date, scope, hash(canonical)].join('\n'));
  assert.equal(req.headers.authorization,
    `AWS4-HMAC-SHA256 Credential=FixtureAccess/${scope}, SignedHeaders=${signed}, Signature=${signature.toString('hex')}`);
}

test('versioned encrypted playback over a disposable HTTPS origin', { timeout: 20_000 }, async (t) => {
  const fixture = make_s3_fixture();
  const ciphertext = Buffer.from(fixture.ciphertext());
  assert.equal(ciphertext.length, 17);
  let response = {};
  let hits = 0;
  let host;
  const server = createServer({
    key: readFileSync(process.env.S3_FIXTURE_KEY),
    cert: readFileSync(process.env.NODE_EXTRA_CA_CERTS),
  }, (req, res) => {
    hits++;
    verifyGet(req, host);
    if (response.stall) res.once('close', response.closed);
    if (response.stall === 'headers') return;
    const headers = {
      'content-range': span,
      'content-length': ciphertext.length,
      'x-amz-version-id': 'version+/%?=&',
      ...response.headers,
    };
    for (const [name, value] of Object.entries(headers)) {
      if (value !== null) res.setHeader(name, value);
    }
    res.writeHead(response.status ?? 206);
    if (response.stall === 'body') {
      res.write(ciphertext.subarray(0, 8));
      return;
    }
    res.end(response.body ?? ciphertext);
  });
  t.after(() => {
    server.closeAllConnections();
    server.close();
    fixture.free();
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  host = `127.0.0.1:${server.address().port}`;
  const endpoint = `https://${host}`;
  const play = (fault = '', timeout = 5000) => fixture.play(endpoint, timeout, fault);
  const refuses = p => assert.rejects(p, error => error === 'storage read refused');

  assert.deepEqual(Buffer.from(await play()), Buffer.from('Z'));
  assert.equal(hits, 1);
  for (const fault of ['provider', 'bucket', 'comma', 'etag', 'stale', 'range']) {
    await refuses(play(fault));
    assert.equal(hits, 1, fault);
  }
  await refuses(fixture.play(`http://${host}`, 5000, ''));
  await refuses(fixture.play(`https://${host}/untrusted-prefix`, 5000, ''));
  assert.equal(hits, 1);

  for (const fault of [
    { status: 302, headers: { location: `${endpoint}/redirected` } },
    { status: 200 },
    { headers: { 'x-amz-version-id': 'wrong-version' } },
    { headers: { 'x-amz-version-id': null } },
    { headers: { 'x-amz-version-id': ['version+/%?=&', 'version+/%?=&'] } },
    { headers: { 'content-range': 'bytes 65599-65615/65617' } },
    { headers: { 'content-range': 'bytes 65600-65616/65618' } },
    { headers: { 'content-range': [span, span] } },
    { headers: { 'content-length': [17, 17] } },
    { headers: { 'content-length': 16 } },
    { headers: { 'content-length': null, 'transfer-encoding': 'chunked' } },
    { headers: { 'content-encoding': 'identity' } },
    { headers: { 'content-encoding': 'gzip' } },
    { headers: { connection: 'close' }, body: ciphertext.subarray(0, 16) },
    { body: Buffer.from(ciphertext.map(byte => byte ^ 1)) },
  ]) {
    response = fault;
    const before = hits;
    await refuses(play());
    assert.equal(hits, before + 1);
  }

  // The origin sees cancellation while the GET or body read is pending.
  for (const [stall, fault] of [
    ['headers', ''], ['body', ''], ['body', 'drop-body'], ['body', 'drop-read'],
  ]) {
    const before = hits;
    const closed = new Promise(resolve => { response = { stall, closed: resolve }; });
    if (fault) {
      assert.equal((await play(fault)).length, 0);
    } else {
      await refuses(play('', 1000));
    }
    await closed;
    assert.equal(hits, before + 1);
  }
});
