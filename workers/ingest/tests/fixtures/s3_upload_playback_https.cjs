'use strict';

const assert = require('node:assert/strict');
const { createHmac } = require('node:crypto');
const { once } = require('node:events');
const { readFileSync } = require('node:fs');
const { createServer } = require('node:https');
const { test } = require('node:test');
const { setImmediate } = require('node:timers/promises');
const loadWorker = require('./worker_fixture_wasm.cjs');
const { upload_for_playback } = loadWorker(process.env.INGEST_WASM_JS, true);
const { resolve_uploaded_playback } = loadWorker(process.env.CATALOGUE_PLAYBACK_WASM_JS, true);
const { fetch: streamFetch } = require('../../../stream/tests/fixtures/s3_stream_wasm.cjs');
const {
  authorization, bucket, checksum, completionReceipt, initiationHeaders,
  initiationReceipt, mediaKey, sha256, sidecarKey,
} = require('./s3_fixture_protocol.cjs');

const mediaPath = `/${bucket}/${mediaKey}`;
const pagePath = `/${bucket}/${sidecarKey}`;
const neighborPath = `${mediaPath}-other`;
const plaintextLength = 65_537;
// SHA-256 of the synthetic FLAC with the padding block used by the uploader.
const plaintextHash = '65ed98d8354c8cd114982e4fc5b3269e411f988b9c1df07e58ebdfea2f09cc2d';
const suffixStart = 65_600;
const sealedLength = 65_617;
const suffixRange = 'bytes=65600-65616';
const fullRange = 'bytes=0-65616';
const original = 'media+/%?=&1';
const replacement = 'media+/%?=&2';
const originalPage = 'page+/%?=&1';
const access = 'FixtureAccess';
const signingMaterial = 'fixture-reader-signing-material';
const queryFor = version => `versionId=${encodeURIComponent(version)}`;

test('Stream plays and seeks the committed FLAC, and refuses invalid pinned responses', { timeout: 30_000 }, async (t) => {
  const neighbor = Buffer.from('unrelated synthetic object');
  const media = new Map();
  const pages = new Map();
  const objects = new Map([
    [mediaPath, media], [pagePath, pages], [neighborPath, new Map([['neighbor-v1', neighbor]])],
  ]);
  const created = [];
  const operations = [];
  let pending;
  let host;
  let rejectedUnsigned = 0;
  let originError;
  let expectedRange;
  let fault;
  let uploads = 0;
  let failCompletion = false;
  let onInterruptedClose;

  function store(versions, version, body) {
    assert.equal(versions.has(version), false);
    versions.set(version, body);
    created.push([versions, version]);
  }

  async function serve(req, res) {
    if (req.headers.authorization === undefined) {
      rejectedUnsigned++;
      res.writeHead(403).end();
      return;
    }
    const chunks = [];
    for await (const chunk of req) chunks.push(chunk);
    const body = Buffer.concat(chunks);
    const [path, query = ''] = req.url.split('?');
    const method = req.method;
    const date = req.headers['x-amz-date'];
    const reader = req.headers.authorization.startsWith(`AWS4-HMAC-SHA256 Credential=${access}/`);
    assert.equal(req.headers.host, host);
    assert.match(date, /^\d{8}T\d{6}Z$/);
    assert.match(req.headers['accept-encoding'], /^identity(?:, identity)?$/);
    assert.ok(path === mediaPath || path === pagePath);

    function verify(extra = {}) {
      const fields = { host, 'x-amz-content-sha256': sha256(body), 'x-amz-date': date, ...extra };
      for (const [name, value] of Object.entries(fields)) assert.equal(req.headers[name], value);
      const [key, secret] = reader
        ? [access, signingMaterial]
        : ['FIXTUREACCESS', 'fixture-secret'];
      assert.equal(req.headers.authorization,
        authorization(method, path, query, fields, key, secret, 'fixture-region'));
    }

    const resource = path === pagePath ? 'page' : reader ? 'range' : 'media';
    operations.push(`${method} ${resource}`);
    if (method === 'POST' && query === 'uploads=') {
      verify(initiationHeaders);
      assert.equal(path, mediaPath);
      assert.equal(pending, undefined);
      pending = { id: `upload+/${++uploads}?` };
      res.end(initiationReceipt(bucket, mediaKey, pending.id));
      return;
    }
    if (method === 'PUT' && path === mediaPath) {
      verify({ 'x-amz-checksum-sha256': checksum(body) });
      assert.equal(query, `partNumber=1&uploadId=${encodeURIComponent(pending.id)}`);
      assert.equal(body.length, sealedLength);
      pending.body = body;
      res.writeHead(200, { etag: '"fixture-part"', 'x-amz-checksum-sha256': checksum(body) }).end();
      return;
    }
    if (method === 'POST') {
      verify({ 'content-type': 'application/xml' });
      assert.equal(path, mediaPath);
      assert.equal(query, `uploadId=${encodeURIComponent(pending.id)}`);
      assert.ok(body.includes(`<ChecksumSHA256>${checksum(pending.body)}</ChecksumSHA256>`));
      if (failCompletion) {
        res.writeHead(503).end();
        return;
      }
      const version = `media+/%?=&${media.size + 1}`;
      const receipt = completionReceipt(body.toString(), 'fixture-part');
      store(media, version, pending.body);
      pending = undefined;
      res.writeHead(200, { 'x-amz-version-id': version }).end(receipt);
      return;
    }
    if (method === 'PUT') {
      verify({ 'x-amz-checksum-sha256': checksum(body) });
      assert.equal(query, '');
      const version = `page+/%?=&${pages.size + 1}`;
      store(pages, version, body);
      res.writeHead(200, { 'x-amz-version-id': version }).end();
      return;
    }

    const versions = objects.get(path);
    const version = query
      ? new URLSearchParams(query).get('versionId')
      : [...versions.keys()].at(-1);
    assert.equal(query, queryFor(version));
    const stored = versions.get(version);
    assert.ok(stored);
    const headers = { 'x-amz-version-id': version, 'content-length': stored.length };
    if (reader && path === mediaPath) {
      verify({ range: expectedRange });
      assert.equal(method, 'GET');
      assert.equal(version, original);
      const start = expectedRange === fullRange ? 0 : suffixStart;
      const bytes = (fault === 'changed bytes' ? media.get(replacement) : stored).subarray(start);
      const rangeHeaders = {
        'x-amz-version-id': fault === 'wrong version' ? replacement : version,
        'content-range': `bytes ${start}-${sealedLength - 1}/${sealedLength}`,
        'content-length': sealedLength - start,
      };
      if (onInterruptedClose) {
        assert.equal(start, 0);
        const closed = onInterruptedClose;
        onInterruptedClose = undefined;
        res.once('close', () => closed(res.writableFinished));
        res.writeHead(206, rangeHeaders).write(bytes.subarray(0, -17));
        return;
      }
      if (fault === 'short range') rangeHeaders.connection = 'close';
      res.writeHead(206, rangeHeaders).end(fault === 'short range' ? bytes.subarray(0, -1) : bytes);
    } else {
      verify();
      assert.equal(req.headers.range, undefined);
      assert.ok(method === 'HEAD' || method === 'GET');
      if (method === 'HEAD') assert.equal(path, mediaPath);
      if (reader) {
        assert.equal(method, 'GET');
        assert.equal(path, pagePath);
        assert.equal(version, originalPage);
      }
      res.writeHead(200, headers).end(method === 'GET' ? stored : undefined);
    }
  }

  const server = createServer({
    key: readFileSync(process.env.S3_FIXTURE_KEY),
    cert: readFileSync(process.env.NODE_EXTRA_CA_CERTS),
  }, (req, res) => {
    serve(req, res).catch(error => {
      originError ??= error;
      res.destroy();
    });
  });
  t.after(async () => {
    await new Promise(resolve => {
      server.close(resolve);
      server.closeAllConnections();
    });
    pending = undefined;
    for (const [versions, version] of created) assert.equal(versions.delete(version), true);
    assert.equal(media.size, 0);
    assert.equal(pages.size, 0);
    assert.deepEqual([...objects.get(neighborPath)], [['neighbor-v1', neighbor]]);
    if (originError) throw originError;
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  host = `127.0.0.1:${server.address().port}`;
  const endpoint = `https://${host}`;

  const track = await upload_for_playback(endpoint, true);
  const upload = ['POST media', 'PUT media', 'POST media', 'HEAD media', 'PUT page', 'GET media', 'GET page'];
  assert.deepEqual(operations.splice(0), upload);

  failCompletion = true;
  await assert.rejects(upload_for_playback(endpoint, false),
    error => error === 'Unconfirmed(Some(CompletionStatus))');
  assert.deepEqual(operations.splice(0), ['POST media', 'PUT media', 'POST media']);
  assert.deepEqual([...media.keys()], [original]);
  assert.deepEqual([...pages.keys()], [originalPage]);
  assert.ok(pending.body, 'the refused completion received a replacement part');

  // Discard the failed upload from the synthetic origin. Open a separate
  // upload for the successful control, with its own provider upload ID.
  pending = undefined;
  failCompletion = false;

  const unsigned = await fetch(`${endpoint}${mediaPath}?${queryFor(original)}`, {
    headers: { range: suffixRange }, signal: AbortSignal.timeout(5000),
  });
  assert.equal(unsigned.status, 403);
  assert.equal(rejectedUnsigned, 1);

  const grantKey = 'synthetic catalogue playback test key';
  const custodyKey = 'synthetic catalogue custody test key';
  const { bearer, play, snapshot } = JSON.parse(await resolve_uploaded_playback(track, grantKey));
  assert.equal(snapshot.media_version, original);
  assert.deepEqual(snapshot.page_versions, [originalPage]);
  const reports = [];
  const custody = [];
  let leaseLive = true;
  class Fetcher {
    async fetch(req) {
      const { origin, pathname } = new URL(req.url);
      const prefix = '/internal/v1/private-playback/';
      assert.equal(origin, 'https://nntune-api');
      assert.equal(req.method, 'POST');
      assert.equal(req.redirect, 'error');
      assert.equal(await req.text(), bearer);
      const signature = createHmac('sha256', custodyKey).update(`${pathname}\n${bearer}`).digest('hex');
      assert.equal(req.headers.get('authorization'), `PlaybackCustody ${signature}`);
      assert.ok([prefix + 'resolve', prefix + 'lease'].includes(pathname));
      custody.push(pathname.slice(prefix.length));
      return pathname === prefix + 'resolve'
        ? Response.json(snapshot)
        : new Response(null, { status: leaseLive ? 204 : 404 });
    }
  }
  class DurableObjectNamespace {
    idFromName(user) { assert.equal(user, 'catalogue-other'); return {}; }
    get() {
      return { fetch: async req => {
        assert.equal(req.url, 'https://nntp-pool/delivered');
        reports.push(await req.json());
        return new Response(null, { status: 202 });
      } };
    }
  }
  // Emulate the Worker's backpressured response stream in Node.
  globalThis.FixedLengthStream = class FixedLengthStream extends TransformStream {
    constructor(length) {
      let count = 0;
      super({
        transform(chunk, controller) {
          count += chunk.length;
          assert.ok(count <= length);
          controller.enqueue(chunk);
        },
        flush() { assert.equal(count, length); },
      });
    }
  };
  t.after(() => { delete globalThis.FixedLengthStream; });
  const env = {
    API: new Fetcher(),
    NNTP_POOL: new DurableObjectNamespace(),
    PLAY_GRANT_SECRET: grantKey,
    PRIVATE_PLAYBACK_S3_CONFIGURATION: '07070707-0707-4007-8007-070707070707',
    PRIVATE_PLAYBACK_S3_BUCKET: bucket,
    PRIVATE_PLAYBACK_S3_ENDPOINT: endpoint,
    PRIVATE_PLAYBACK_S3_REGION: 'fixture-region',
    PRIVATE_PLAYBACK_ROOT_VERSION: '00000000-0000-4000-8000-000000000000',
    PRIVATE_PLAYBACK_ROOT_KEY: '11'.repeat(32),
    PRIVATE_PLAYBACK_CUSTODY_SECRET: custodyKey,
    PRIVATE_PLAYBACK_S3_ACCESS_KEY_ID: access,
    PRIVATE_PLAYBACK_S3_SECRET_ACCESS_KEY: signingMaterial,
  };
  async function open(range, enabled = true) {
    const pending = [];
    const req = new Request(`https://stream.test/play/${track}`, {
      headers: { authorization: `Bearer ${bearer}`, ...(range && { range }) },
    });
    const response = await streamFetch(req,
      enabled ? { ...env, PRIVATE_PLAYBACK_ENABLED: 'true' } : env,
      { waitUntil: promise => pending.push(promise) });
    return { response, signal: req.signal, settled: Promise.all(pending) };
  }
  async function request(range, enabled = true) {
    const { response, settled } = await open(range, enabled);
    const bytes = Buffer.from(await response.arrayBuffer());
    await settled;
    return { response, bytes };
  }

  const disabled = await request(undefined, false);
  assert.equal(disabled.response.status, 404);
  assert.deepEqual(custody, []);
  assert.deepEqual(operations, []);

  const seek = 'bytes=65536-65536';
  const interval = { user_id: 'catalogue-other', play_id: play, track_id: track, file_bytes: plaintextLength };
  // Finish before the media read deadline could close the origin on its own.
  await t.test('an interrupted encrypted range settles only its prefix and a new seek rechecks custody',
    { timeout: 4000 }, async () => {
      expectedRange = fullRange;
      const { promise: closed, resolve } = Promise.withResolvers();
      onInterruptedClose = resolve;
      const interrupted = await open('bytes=65535-65536');
      assert.equal(interrupted.response.status, 206);
      assert.equal(interrupted.response.headers.get('content-range'), 'bytes 65535-65536/65537');
      const client = interrupted.response.body.getReader();
      const first = await client.read();
      assert.equal(first.done, false);
      assert.deepEqual(Buffer.from(first.value), Buffer.from([0x9d]));
      // Let the Worker acknowledge the first byte and start awaiting the withheld frame.
      await setImmediate();
      await client.cancel();
      assert.equal(interrupted.signal.aborted, false);
      assert.equal(await closed, false);
      await interrupted.settled;
      assert.deepEqual(operations.splice(0), ['GET range', 'GET page']);
      assert.deepEqual(custody.splice(0), ['resolve', 'lease', 'lease']);
      const prefix = { ...interval, start: 65_535, end: 65_536 };
      const tail = { ...interval, start: 65_536, end: 65_537 };
      assert.deepEqual(reports, [prefix]);

      expectedRange = suffixRange;
      const resumed = await request(seek);
      assert.equal(resumed.response.status, 206);
      assert.equal(resumed.response.headers.get('content-range'), 'bytes 65536-65536/65537');
      assert.deepEqual(resumed.bytes, Buffer.from([0x1d]));
      assert.deepEqual(operations.splice(0), ['GET range', 'GET page']);
      assert.deepEqual(custody.splice(0), ['resolve', 'lease', 'lease']);
      assert.deepEqual(reports, [prefix, tail]);

      leaseLive = false;
      const refused = await request(seek);
      assert.equal(refused.response.status, 404);
      assert.deepEqual(custody.splice(0), ['resolve', 'lease']);
      assert.deepEqual(operations, []);
      assert.equal(rejectedUnsigned, 1, 'only the explicit unsigned control reached the origin');
      assert.deepEqual(reports.splice(0), [prefix, tail]);
      leaseLive = true;
    });

  expectedRange = fullRange;
  const full = await request();
  assert.equal(full.response.status, 200);
  assert.equal(full.response.headers.get('content-type'), 'audio/flac');
  assert.equal(full.response.headers.get('content-range'), null);
  assert.equal(full.bytes.length, plaintextLength);
  assert.equal(full.bytes.toString('ascii', 0, 4), 'fLaC');
  assert.equal(full.bytes[4] & 0x7f, 0, 'the first metadata block is STREAMINFO');
  assert.equal(full.bytes.readUIntBE(5, 3), 34, 'STREAMINFO payload length');
  // STREAMINFO packs the sample format and count into one big-endian word.
  const streamInfo = full.bytes.readBigUInt64BE(18);
  assert.equal(Number(streamInfo >> 44n), 44_100, 'sample rate in Hz');
  assert.equal(Number((streamInfo >> 41n) & 7n) + 1, 1, 'channels');
  assert.equal(Number((streamInfo >> 36n) & 31n) + 1, 8, 'bits per sample');
  assert.equal(Number(streamInfo & ((1n << 36n) - 1n)), 19, 'total samples');
  assert.equal(sha256(full.bytes), plaintextHash);
  assert.deepEqual(operations.splice(0), ['GET range', 'GET page']);
  assert.deepEqual(custody.splice(0), ['resolve', 'lease', 'lease', 'lease']);

  expectedRange = suffixRange;
  const partial = await request(seek);
  assert.equal(partial.response.status, 206);
  assert.equal(partial.response.headers.get('content-range'), 'bytes 65536-65536/65537');
  assert.deepEqual(partial.bytes, Buffer.from([0x1d]));
  assert.deepEqual(operations.splice(0), ['GET range', 'GET page']);
  assert.deepEqual(custody.splice(0), ['resolve', 'lease', 'lease']);

  // The padded FLAC ends one byte into the second 64 KiB plaintext chunk.
  // Stream fetches the header and both sealed chunks for this selection.
  expectedRange = fullRange;
  const boundary = await request('bytes=65535-65536');
  assert.equal(boundary.response.status, 206);
  assert.equal(boundary.response.headers.get('content-range'), 'bytes 65535-65536/65537');
  assert.deepEqual(boundary.bytes, Buffer.from([0x9d, 0x1d]));
  assert.deepEqual(operations.splice(0), ['GET range', 'GET page']);
  assert.deepEqual(custody.splice(0), ['resolve', 'lease', 'lease', 'lease']);

  assert.deepEqual(reports.splice(0), [
    { ...interval, start: 0, end: plaintextLength },
    { ...interval, start: plaintextLength - 1, end: plaintextLength },
    { ...interval, start: plaintextLength - 2, end: plaintextLength },
  ]);

  assert.equal(await upload_for_playback(endpoint, false), replacement);
  assert.equal(uploads, 3);
  assert.deepEqual(operations.splice(0), upload);
  const current = [...media.entries()].at(-1);
  assert.equal(current[0], replacement);
  assert.notDeepEqual(current[1].subarray(suffixStart), media.get(original).subarray(suffixStart));
  expectedRange = suffixRange;
  const pinned = await request(seek);
  assert.equal(pinned.response.status, 206);
  assert.deepEqual(pinned.bytes, Buffer.from([0x1d]));
  assert.deepEqual(operations.splice(0), ['GET range', 'GET page']);
  assert.deepEqual(custody.splice(0), ['resolve', 'lease', 'lease']);
  assert.deepEqual(reports.splice(0), [{ ...interval, start: plaintextLength - 1, end: plaintextLength }]);

  for (fault of ['wrong version', 'changed bytes', 'short range']) {
    const refused = await request(seek);
    assert.equal(refused.response.status, 502, fault);
    assert.match(refused.response.headers.get('content-type'), /^application\/json/, fault);
    assert.deepEqual(operations.splice(0),
      fault === 'wrong version' ? ['GET range'] : ['GET range', 'GET page'], fault);
    assert.deepEqual(custody.splice(0), ['resolve', 'lease'], fault);
    assert.deepEqual(reports, [], fault);
  }
});
