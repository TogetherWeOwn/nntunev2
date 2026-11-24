'use strict';

const assert = require('node:assert/strict');
const { once } = require('node:events');
const { readFileSync } = require('node:fs');
const { createServer } = require('node:https');
const { test } = require('node:test');
const loadWorker = require('./worker_fixture_wasm.cjs');
const { upload_for_playback } = loadWorker(process.env.INGEST_WASM_JS, true);
const { resolve_uploaded_playback } = loadWorker(process.env.CATALOGUE_PLAYBACK_WASM_JS, true);
const { read_uploaded_suffix } = require('../../../stream/tests/fixtures/s3_stream_wasm.cjs');
const {
  authorization, bucket, checksum, completionReceipt, initiationHeaders,
  initiationReceipt, mediaKey, sha256, sidecarKey,
} = require('./s3_fixture_protocol.cjs');

const mediaPath = `/${bucket}/${mediaKey}`;
const pagePath = `/${bucket}/${sidecarKey}`;
const neighborPath = `${mediaPath}-other`;
const start = 65_600;
const length = 65_617;
const range = 'bytes=65600-65616';
const queryFor = version => `versionId=${encodeURIComponent(version)}`;

test('reads the committed catalogue version after the current storage version changes', { timeout: 30_000 }, async (t) => {
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
    const reader = req.headers.range !== undefined;
    assert.equal(req.headers.host, host);
    assert.match(date, /^\d{8}T\d{6}Z$/);
    assert.match(req.headers['accept-encoding'], /^identity(?:, identity)?$/);
    assert.ok(path === mediaPath || path === pagePath);

    function verify(extra = {}) {
      const fields = { host, 'x-amz-content-sha256': sha256(body), 'x-amz-date': date, ...extra };
      for (const [name, value] of Object.entries(fields)) assert.equal(req.headers[name], value);
      const [access, secret] = reader
        ? ['FixtureAccess', 'fixture-reader-signing-material']
        : ['FIXTUREACCESS', 'fixture-secret'];
      assert.equal(req.headers.authorization,
        authorization(method, path, query, fields, access, secret, 'fixture-region'));
    }

    operations.push(`${method} ${reader ? 'range' : path === pagePath ? 'page' : 'media'}`);
    if (method === 'POST' && query === 'uploads=') {
      verify(initiationHeaders);
      assert.equal(path, mediaPath);
      assert.equal(pending, undefined);
      pending = { id: `upload+/${media.size + 1}?` };
      res.end(initiationReceipt(bucket, mediaKey, pending.id));
      return;
    }
    if (method === 'PUT' && path === mediaPath) {
      verify({ 'x-amz-checksum-sha256': checksum(body) });
      assert.equal(query, `partNumber=1&uploadId=${encodeURIComponent(pending.id)}`);
      assert.equal(body.length, length);
      pending.body = body;
      res.writeHead(200, { etag: '"fixture-part"', 'x-amz-checksum-sha256': checksum(body) }).end();
      return;
    }
    if (method === 'POST') {
      verify({ 'content-type': 'application/xml' });
      assert.equal(path, mediaPath);
      assert.equal(query, `uploadId=${encodeURIComponent(pending.id)}`);
      assert.ok(body.includes(`<ChecksumSHA256>${checksum(pending.body)}</ChecksumSHA256>`));
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
    if (reader) {
      verify({ range });
      assert.equal(method, 'GET');
      assert.equal(path, mediaPath);
      res.writeHead(206, {
        ...headers, 'content-range': `bytes 65600-65616/${length}`, 'content-length': length - start,
      }).end(stored.subarray(start, length));
    } else {
      verify();
      assert.ok(method === 'HEAD' || method === 'GET');
      if (method === 'HEAD') assert.equal(path, mediaPath);
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
  const replacement = await upload_for_playback(endpoint, false);
  const original = 'media+/%?=&1';
  assert.equal(replacement, 'media+/%?=&2');
  const expected = media.get(original).subarray(start);
  const current = [...media.entries()].at(-1);
  assert.equal(current[0], replacement);
  assert.notDeepEqual(current[1].subarray(start), expected);

  const unsigned = await fetch(`${endpoint}${mediaPath}?${queryFor(original)}`, {
    headers: { range }, signal: AbortSignal.timeout(5000),
  });
  assert.equal(unsigned.status, 403);
  assert.equal(rejectedUnsigned, 1);
  const pointer = await resolve_uploaded_playback(track);
  assert.equal(JSON.parse(pointer).s3_version_id, original);
  assert.deepEqual(Buffer.from(await read_uploaded_suffix(endpoint, pointer)), expected);
  const upload = ['POST media', 'PUT media', 'POST media', 'HEAD media', 'PUT page', 'GET media', 'GET page'];
  assert.deepEqual(operations, [...upload, ...upload, 'GET range']);
});
