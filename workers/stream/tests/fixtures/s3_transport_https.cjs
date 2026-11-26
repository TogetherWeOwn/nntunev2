// Loopback S3-shaped origin for the unmounted Wasm playback transport.
const assert = require('node:assert/strict');
const { createHash, createHmac } = require('node:crypto');
const { once } = require('node:events');
const { readFileSync } = require('node:fs');
const { createServer } = require('node:https');
const { test } = require('node:test');
const { setImmediate } = require('node:timers/promises');
const { make_s3_fixture, fetch: streamFetch } = require('./s3_stream_wasm.cjs');

const path = '/fixture-media/media/00000000000040008000000000000003/00000000000040008000000000000004';
const query = 'versionId=version%2B%2F%25%3F%3D%26';
const span = 'bytes 65600-65616/65617';
const shiftedSpan = 'bytes 65599-65615/65617';
const pagePath = path.replace('/media/', '/media-digests/') + '/00000000000040008000000000000006/00000000';
const pageQuery = 'versionId=page%2B%2F%25%3F%3D%26';
const hash = (value) => createHash('sha256').update(value).digest('hex');
const mac = (key, value) => createHmac('sha256', key).update(value).digest();

function verifyGet(req, host, page = false, range = 'bytes=65600-65616') {
  const objectPath = page ? pagePath : path;
  const versionQuery = page ? pageQuery : query;
  assert.equal(req.method, 'GET');
  assert.equal(req.url, `${objectPath}?${versionQuery}`);
  assert.equal(req.headers.host, host);
  if (page) assert.equal(req.headers.range, undefined);
  else assert.equal(req.headers.range, range);
  // Undici also appends identity automatically for a ranged request.
  assert.match(req.headers['accept-encoding'], /^identity(?:, identity)?$/);
  assert.equal(req.headers['x-amz-content-sha256'], hash(''));
  const date = req.headers['x-amz-date'];
  assert.match(date, /^\d{8}T\d{6}Z$/);
  const signed = page ? 'host;x-amz-content-sha256;x-amz-date' : 'host;range;x-amz-content-sha256;x-amz-date';
  const headers = signed.split(';').map(key => `${key}:${req.headers[key]}\n`).join('');
  const canonical = ['GET', objectPath, versionQuery, headers, signed, hash('')].join('\n');
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
    { headers: { 'content-range': shiftedSpan } },
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

test('versioned digest pages require exact complete response framing', { timeout: 10_000 }, async (t) => {
  const fixture = make_s3_fixture();
  const page = Buffer.from(fixture.digest_page());
  let host;
  let fault = {};
  const server = createServer({
    key: readFileSync(process.env.S3_FIXTURE_KEY),
    cert: readFileSync(process.env.NODE_EXTRA_CA_CERTS),
  }, (req, res) => {
    verifyGet(req, host, true);
    res.writeHead(fault.status ?? 200, {
      'content-length': page.length,
      'x-amz-version-id': 'page+/%?=&',
      ...fault.headers,
    });
    if (fault.closed) {
      res.once('close', fault.closed);
      res.write(page.subarray(1));
      return;
    }
    res.end(fault.body ?? page);
  });
  t.after(() => {
    server.closeAllConnections();
    server.close();
    fixture.free();
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  host = `127.0.0.1:${server.address().port}`;
  const read = () => fixture.read_digest_page(`https://${host}`);
  assert.deepEqual(Buffer.from(await read()), page);
  for (fault of [
    { status: 206 },
    { headers: { 'content-range': 'bytes 0-155/156' } },
    { headers: { 'x-amz-version-id': ['page+/%?=&', 'page+/%?=&'] } },
    { headers: { 'x-amz-version-id': 'wrong-version' } },
    { headers: { 'content-encoding': 'identity' } },
    { headers: { 'content-length': page.length - 1 }, body: page.subarray(1) },
    { headers: { connection: 'close' }, body: page.subarray(1) },
  ]) await assert.rejects(read(), error => error === 'storage read refused');

  const closed = new Promise(resolve => { fault = { closed: resolve }; });
  await assert.rejects(read(), error => error === 'storage read refused');
  await closed;
});

test('Stream fetch serves a pinned encrypted seek and settles only written bytes', { timeout: 20_000 }, async (t) => {
  const fixture = make_s3_fixture();
  const plaintext = Buffer.concat([Buffer.alloc(65_534, 'A'), Buffer.from('XYZ')]);
  const expires = Math.floor(Date.now() / 1000) + 90;
  const grantKey = 'fixture-only-play-grant-key-material';
  const custodyKey = 'fixture-only-transport-authentication-key';
  const play = 'private_1234567890abcdefghijkl';
  const payload = Buffer.from(`member|track|${play}|42|${expires}|${hash(plaintext)}`).toString('base64url');
  const signed = `v4.${payload}`;
  const bearer = `${signed}.${mac(grantKey, signed).toString('base64url')}`;
  const original = JSON.parse(fixture.custody(bearer, expires));
  const sealedLength = fixture.full_ciphertext().length;
  let reportedSealedLength = sealedLength;
  let snapshot = original;
  let resolveFault;
  let leaseNo = 0;
  let revokeAt = Infinity;
  let host;
  let mediaVersionOverride;
  let observedPageVersion = 'page+/%?=&';
  let seekContentRange = span;
  let fullRange = false;
  let stallTail;
  const fetches = [];
  const reports = [];
  const server = createServer({
    key: readFileSync(process.env.S3_FIXTURE_KEY),
    cert: readFileSync(process.env.NODE_EXTRA_CA_CERTS),
  }, (req, res) => {
    const page = req.url.startsWith(pagePath);
    verifyGet(req, host, page, fullRange ? 'bytes=0-65616' : undefined);
    fetches.push(page ? 'page' : 'media');
    const bytes = Buffer.from(page ? fixture.digest_page()
      : fullRange ? fixture.full_ciphertext() : fixture.ciphertext());
    const headers = {
      'content-length': bytes.length,
      'x-amz-version-id': page ? observedPageVersion : (mediaVersionOverride ?? 'version+/%?=&'),
    };
    if (!page) headers['content-range'] = fullRange ? `bytes 0-65616/${reportedSealedLength}` : seekContentRange;
    res.writeHead(page ? 200 : 206, headers);
    if (!page && stallTail) {
      res.once('close', () => stallTail(res.writableFinished));
      res.write(bytes.subarray(0, -17));
      return;
    }
    res.end(bytes);
  });
  t.after(() => {
    server.closeAllConnections();
    server.close();
    fixture.free();
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  host = `127.0.0.1:${server.address().port}`;

  const custody = [];
  class Fetcher {
    async fetch(req) {
      const { origin, pathname } = new URL(req.url);
      assert.equal(origin, 'https://nntune-api');
      assert.equal(req.method, 'POST');
      assert.equal(req.redirect, 'error');
      assert.equal(await req.text(), bearer);
      assert.equal(req.headers.get('authorization'),
        `PlaybackCustody ${mac(custodyKey, `${pathname}\n${bearer}`).toString('hex')}`);
      custody.push(pathname);
      if (pathname.endsWith('/resolve')) {
        if (resolveFault === 'oversize') {
          return new Response(JSON.stringify(snapshot) + ' '.repeat(2 * 1024 * 1024));
        }
        if (resolveFault === 'stall') {
          return new Promise((_, reject) => {
            req.signal.addEventListener('abort', () => reject(req.signal.reason), { once: true });
          });
        }
        return Response.json(snapshot);
      }
      return new Response(null, { status: ++leaseNo >= revokeAt ? 404 : 204 });
    }
  }
  class DurableObjectNamespace {
    idFromName(user) { assert.equal(user, 'member'); return {}; }
    get() {
      return { fetch: async req => {
        assert.equal(req.url, 'https://nntp-pool/delivered');
        reports.push(await req.json());
        return new Response(null, { status: 202 });
      } };
    }
  }
  // Preserve backpressure and reject writes that violate the declared length.
  globalThis.FixedLengthStream = class FixedLengthStream extends TransformStream {
    constructor(len) {
      let count = 0;
      super({
        transform(chunk, controller) {
          count += chunk.length;
          assert.ok(count <= len);
          controller.enqueue(chunk);
        },
        flush() { assert.equal(count, len); },
      });
    }
  };
  t.after(() => { delete globalThis.FixedLengthStream; });
  const env = {
    API: new Fetcher(),
    NNTP_POOL: new DurableObjectNamespace(),
    PLAY_GRANT_SECRET: grantKey,
    PRIVATE_PLAYBACK_ENABLED: 'true',
    PRIVATE_PLAYBACK_S3_CONFIGURATION: '00000000-0000-4000-8000-000000000005',
    PRIVATE_PLAYBACK_S3_BUCKET: 'fixture-media',
    PRIVATE_PLAYBACK_S3_ENDPOINT: `https://${host}`,
    PRIVATE_PLAYBACK_S3_REGION: 'fixture-1',
    PRIVATE_PLAYBACK_ROOT_VERSION: '00000000-0000-4000-8000-000000000007',
    PRIVATE_PLAYBACK_ROOT_KEY: '07'.repeat(32),
    PRIVATE_PLAYBACK_CUSTODY_SECRET: custodyKey,
    PRIVATE_PLAYBACK_S3_ACCESS_KEY_ID: 'FixtureAccess',
    PRIVATE_PLAYBACK_S3_SECRET_ACCESS_KEY: 'fixture-reader-signing-material',
  };
  async function open(overrides, suffix, token, headers) {
    const pending = [];
    const req = new Request(`https://stream.test${suffix}`, {
      headers: { authorization: `Bearer ${token}`, ...headers },
    });
    const response = await streamFetch(req, { ...env, ...overrides }, { waitUntil: p => pending.push(p) });
    return { req, response, settled: Promise.all(pending) };
  }
  async function request(overrides = {}, suffix = '/play/track', token = bearer, headers = { range: 'bytes=65536-65536' }) {
    const { response, settled } = await open(overrides, suffix, token, headers);
    const bytes = Buffer.from(await response.arrayBuffer());
    await settled;
    return { response, bytes };
  }

  const { response, bytes } = await request();
  assert.equal(response.status, 206);
  assert.equal(response.headers.get('content-range'), 'bytes 65536-65536/65537');
  assert.equal(response.headers.get('cache-control'), 'private, no-store');
  assert.deepEqual(bytes, plaintext.subarray(65_536));
  assert.deepEqual(fetches, ['media', 'page']);
  assert.deepEqual(custody, ['resolve', 'lease', 'lease'].map(x => `/internal/v1/private-playback/${x}`));
  assert.deepEqual(reports, [{ user_id: 'member', play_id: play, track_id: 'track', start: 65_536, end: 65_537, file_bytes: 65_537 }]);

  const status = async (overrides, suffix, token) => (await request(overrides, suffix, token)).response.status;
  assert.equal(await status({}, '/health'), 404);
  assert.equal(await status({}, '/play/other'), 403);
  assert.equal(await status({}, '/play/track', 'v4.invalid'), 401);
  for (const binding of Object.keys(env).filter(x => x.startsWith('PRIVATE_PLAYBACK_')).concat('API', 'NNTP_POOL')) {
    assert.equal(await status({ [binding]: undefined }), 404, binding);
  }
  assert.equal(await status({ PRIVATE_PLAYBACK_ENABLED: 'false' }), 404);
  assert.equal(custody.length, 3);
  for (const mismatch of [
    { bearer_sha256: Array(32).fill(0) },
    { catalogue_reference: Array(16).fill(0) },
    { bucket: 'other-media' },
    { media_version: 'version,duplicate' },
    { page_versions: ['null'] },
    { envelope_record: original.envelope_record.map((x, i) => i === 229 ? x ^ 1 : x) },
  ]) {
    snapshot = { ...original, ...mismatch };
    assert.equal(await status(), 404);
  }
  snapshot = original;
  assert.equal(await status({ PRIVATE_PLAYBACK_ROOT_KEY: '08'.repeat(32) }), 404);
  assert.equal(await status({ PRIVATE_PLAYBACK_ROOT_VERSION: '00000000-0000-4000-8000-000000000008' }), 404);
  assert.equal(await status({ PRIVATE_PLAYBACK_S3_CONFIGURATION: '00000000-0000-4000-8000-000000000008' }), 404);
  for (resolveFault of ['oversize', 'stall']) assert.equal(await status(), 404, resolveFault);
  resolveFault = undefined;
  revokeAt = leaseNo + 1;
  assert.equal(await status(), 404);
  assert.equal(fetches.length, 2);
  revokeAt = leaseNo + 2;
  assert.equal(await status(), 404);
  assert.equal(fetches.length, 4);
  revokeAt = Infinity;

  await t.test('a mismatched media version delivers and counts no media', async (t) => {
    const fetchStart = fetches.length;
    const reportStart = reports.length;
    // Use the same status, range, length and ciphertext as the successful seek.
    mediaVersionOverride = 'wrong-version';
    t.after(() => { mediaVersionOverride = undefined; });
    const refused = await request();
    assert.equal(refused.response.status, 502);
    assert.equal(refused.response.headers.get('content-range'), null);
    assert.equal(JSON.parse(refused.bytes).code, 'upstream_unavailable');
    assert.deepEqual(fetches.slice(fetchStart), ['media']);
    assert.equal(reports.length, reportStart);
  });

  await t.test('a surplus byte in a pinned media body delivers and counts no media', async (t) => {
    const fetchStart = fetches.length;
    const reportStart = reports.length;
    const fetch = globalThis.fetch;
    let appendSurplus = true;
    t.mock.method(globalThis, 'fetch', async (req) => {
      const response = await fetch(req);
      if (!appendSurplus || req.url !== `https://${host}${path}?${query}`) return response;
      appendSurplus = false;
      assert.equal(response.status, 206);
      assert.equal(response.headers.get('content-range'), span);
      assert.equal(response.headers.get('x-amz-version-id'), 'version+/%?=&');
      assert.equal(response.headers.get('content-length'), '17');
      // Model the surplus at the Fetch boundary after HTTPS has framed the body.
      const body = response.body.pipeThrough(new TransformStream({
        flush(controller) { controller.enqueue(Uint8Array.of(0)); },
      }));
      return new Response(body, { status: response.status, headers: response.headers });
    });

    const refused = await request();
    assert.equal(refused.response.status, 502);
    assert.equal(refused.response.headers.get('content-range'), null);
    const error = JSON.parse(refused.bytes);
    assert.equal(error.code, 'upstream_unavailable');
    assert.equal(error.retryable, true);
    assert.deepEqual(fetches.slice(fetchStart), ['media', 'page']);
    assert.deepEqual(reports.slice(reportStart), []);

    const control = await request();
    assert.equal(control.response.status, 206);
    assert.equal(control.response.headers.get('content-range'), 'bytes 65536-65536/65537');
    assert.deepEqual(control.bytes, plaintext.subarray(65_536));
    assert.deepEqual(fetches.slice(fetchStart), ['media', 'page', 'media', 'page']);
    assert.deepEqual(reports.slice(reportStart), [{
      user_id: 'member', play_id: play, track_id: 'track', start: 65_536, end: 65_537, file_bytes: 65_537,
    }]);
  });

  const pageFaultStart = fetches.length;
  const pageReportStart = reports.length;
  observedPageVersion = 'wrong-version';
  assert.equal(await status(), 502);
  assert.deepEqual(fetches.slice(pageFaultStart), ['media', 'page']);
  assert.equal(reports.length, pageReportStart);
  observedPageVersion = 'page+/%?=&';

  await t.test('a shifted Content-Range is refused and the same request can be retried', async (t) => {
    const fetchStart = fetches.length;
    const reportStart = reports.length;
    // Keep the status, version, length and authentic ciphertext from the successful seek.
    seekContentRange = shiftedSpan;
    t.after(() => { seekContentRange = span; });
    const refused = await request();
    assert.equal(refused.response.status, 502);
    assert.equal(refused.response.headers.get('content-range'), null);
    const error = JSON.parse(refused.bytes);
    assert.equal(error.code, 'upstream_unavailable');
    assert.equal(error.retryable, true);
    assert.deepEqual(fetches.slice(fetchStart), ['media']);
    assert.equal(reports.length, reportStart);

    seekContentRange = span;
    const retry = await request();
    assert.equal(retry.response.status, 206);
    assert.equal(retry.response.headers.get('content-range'), 'bytes 65536-65536/65537');
    assert.deepEqual(retry.bytes, plaintext.subarray(65_536));
    assert.deepEqual(fetches.slice(fetchStart), ['media', 'media', 'page']);
    assert.deepEqual(reports.slice(reportStart), [{
      user_id: 'member', play_id: play, track_id: 'track', start: 65_536, end: 65_537, file_bytes: 65_537,
    }]);
  });

  fullRange = true;
  const boundedSeek = { range: 'bytes=65534-65540' };
  await t.test('a bounded range crossing the short final chunk ends at media EOF', { timeout: 5_000 }, async () => {
    const firstFetch = fetches.length;
    const firstReport = reports.length;
    // request() drains the body to EOF before returning.
    const { response, bytes } = await request({}, '/play/track', bearer, boundedSeek);
    assert.equal(response.status, 206);
    assert.equal(response.headers.get('content-range'), 'bytes 65534-65536/65537');
    assert.deepEqual(bytes, Buffer.from('XYZ'));
    assert.deepEqual(fetches.slice(firstFetch), ['media', 'page']);
    assert.deepEqual(reports.slice(firstReport), [{
      user_id: 'member', play_id: play, track_id: 'track', start: 65_534, end: 65_537, file_bytes: 65_537,
    }]);
  });

  await t.test('a wrong encrypted object length delivers and counts no media', { timeout: 5_000 }, async (t) => {
    const firstFetch = fetches.length;
    const firstReport = reports.length;
    reportedSealedLength = sealedLength + 1;
    t.after(() => { reportedSealedLength = sealedLength; });

    const { response, bytes } = await request({}, '/play/track', bearer, boundedSeek);
    assert.equal(response.status, 502);
    assert.equal(response.headers.get('content-range'), null);
    assert.equal(JSON.parse(bytes).code, 'upstream_unavailable');
    assert.deepEqual(fetches.slice(firstFetch), ['media']);
    assert.equal(reports.length, firstReport);
  });

  await t.test('canceling a v4 response stops a pending read at the HTTPS origin', { timeout: 5_000 }, async (t) => {
    const logs = t.mock.method(console, 'log', () => {});
    const trace = '0123456789abcdef0123456789abcdef';
    const setting = `${trace}:${hash('track')}`;
    const stage = { ENVIRONMENT: 'staging', PRIVATE_PLAYBACK_READ_OBSERVATION: setting };
    const headers = { range: 'bytes=0-65536', 'x-trace-id': trace };
    for (const [overrides, inbound] of [
      [{ ENVIRONMENT: 'staging' }, headers],
      [{ ...stage, ENVIRONMENT: 'production' }, headers],
      [{ ...stage, PRIVATE_PLAYBACK_READ_OBSERVATION: `${trace}:${hash('other-track')}` }, headers],
      [stage, { range: headers.range }],
    ]) assert.deepEqual((await request(overrides, '/play/track', bearer, inbound)).bytes, plaintext);
    assert.equal(logs.mock.callCount(), 0);

    const events = () => logs.mock.calls.map(({ arguments: args }) => {
      assert.equal(args.length, 1);
      const { ts, event, ...fields } = JSON.parse(args[0]);
      assert.ok(Number.isFinite(Date.parse(ts)));
      assert.deepEqual(fields, { worker: 'stream', level: 'info', trace_id: trace, fields: {} });
      return event;
    });
    const started = 'private_storage_read_started';
    const completed = 'private_storage_read_completed';
    const aborted = 'private_storage_read_aborted';
    assert.deepEqual((await request(stage, '/play/track', bearer, headers)).bytes, plaintext);
    assert.deepEqual(events(), [started, completed]);

    const closed = new Promise(resolve => { stallTail = resolve; });
    const { req, response, settled } = await open(stage, '/play/track', bearer, headers);
    assert.equal(response.status, 206);
    const client = response.body.getReader();
    const first = await client.read();
    assert.equal(first.done, false);
    assert.deepEqual(Buffer.from(first.value), plaintext.subarray(0, 65_536));
    await setImmediate();
    assert.deepEqual(events(), [started, completed, started]);
    await client.cancel();
    // Reader cancellation leaves the inbound Request signal untouched. The
    // origin must close before sending the final frame and before expiry.
    assert.equal(req.signal.aborted, false);
    assert.equal(await closed, false);
    await settled;
    assert.deepEqual(events(), [started, completed, started, aborted]);
    assert.deepEqual(reports.at(-1), {
      user_id: 'member', play_id: play, track_id: 'track', start: 0, end: 65_536, file_bytes: 65_537,
    });
  });
});
