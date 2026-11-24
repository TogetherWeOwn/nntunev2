'use strict';

const assert = require('node:assert/strict');
const { createHash, createHmac } = require('node:crypto');
const { test } = require('node:test');
const { s3_completion_test: complete } = require('./ingest_wasm.cjs');

const version = 'fixture-version';
const bucket = 'fixture-bucket';
const mediaKey = 'media/abababababab40ab80ababababababab/cdcdcdcdcdcd40cd80cdcdcdcdcdcdcd';
const transportFailure = 'Unconfirmed(None)';
const rejectedWith = (expected) => (error) => error === expected;
const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');
const mac = (key, text) => createHmac('sha256', key).update(text).digest();

function completionReceipt(manifest) {
  const prefix = '<CompleteMultipartUpload xmlns="http://s3.amazonaws.com/doc/2006-03-01/">'
    + '<Part><PartNumber>1</PartNumber><ETag>&quot;fixture-etag&quot;</ETag><ChecksumSHA256>';
  const suffix = '</ChecksumSHA256></Part></CompleteMultipartUpload>';
  assert.ok(manifest.startsWith(prefix));
  assert.ok(manifest.endsWith(suffix));
  const part = manifest.slice(prefix.length, -suffix.length);
  assert.match(part, /^[A-Za-z0-9+/]{43}=$/);
  const composite = createHash('sha256').update(Buffer.from(part, 'base64')).digest('base64');
  return `<CompleteMultipartUploadResult><Bucket>${bucket}</Bucket><Key>${mediaKey}</Key>`
    + `<ChecksumSHA256>${composite}-1</ChecksumSHA256></CompleteMultipartUploadResult>`;
}

test('signs the exact multipart POST once and collects its version and body', async (t) => {
  const fetch = t.mock.method(globalThis, 'fetch', async (request) => {
    const url = new URL(request.url);
    const path = `/${bucket}/${mediaKey}`;
    const query = 'uploadId=fixture%2B%2F%3D%3F';
    assert.equal(url.href, `https://fixture.test:8443${path}?${query}`);
    assert.equal(request.method, 'POST');
    assert.equal(request.redirect, 'manual');
    assert.equal(request.cache, 'no-store');
    assert.equal(request.headers.get('accept-encoding'), 'identity');
    const body = await request.text();
    const payload = sha256(body);
    assert.equal(request.headers.get('content-type'), 'application/xml');
    assert.equal(request.headers.get('x-amz-content-sha256'), payload);
    const date = request.headers.get('x-amz-date');
    assert.match(date, /^\d{8}T\d{6}Z$/);
    const day = date.slice(0, 8);
    const scope = `${day}/fixture-region/s3/aws4_request`;
    const signedHeaders = 'content-type;host;x-amz-content-sha256;x-amz-date';
    const canonical = [
      'POST', path, query, 'content-type:application/xml', 'host:fixture.test:8443',
      `x-amz-content-sha256:${payload}`, `x-amz-date:${date}`, '', signedHeaders, payload,
    ].join('\n');
    const key = [day, 'fixture-region', 's3', 'aws4_request'].reduce(mac, 'AWS4fixture-secret');
    const signature = mac(key, `AWS4-HMAC-SHA256\n${date}\n${scope}\n${sha256(canonical)}`).toString('hex');
    assert.equal(request.headers.get('authorization'),
      `AWS4-HMAC-SHA256 Credential=FIXTUREACCESS/${scope}, SignedHeaders=${signedHeaders}, Signature=${signature}`);
    return new Response(completionReceipt(body), { headers: { 'x-amz-version-id': version } });
  });
  assert.equal(await complete('success'), version);
  assert.equal(fetch.mock.callCount(), 1);
});

test('requires a canonical HTTPS origin before fetch', async (t) => {
  const fetch = t.mock.method(globalThis, 'fetch', () => { throw new Error('unexpected fetch'); });
  for (const mode of ['http', 'path']) {
    await assert.rejects(complete(mode), rejectedWith('invalid configuration'));
  }
  assert.equal(fetch.mock.callCount(), 0);
});

test('refuses a foreign configuration or bucket or an expired deadline before fetch', async (t) => {
  const fetch = t.mock.method(globalThis, 'fetch', () => { throw new Error('unexpected fetch'); });
  for (const mode of ['foreign', 'bucket', 'expired']) {
    await assert.rejects(complete(mode), rejectedWith(transportFailure));
  }
  assert.equal(fetch.mock.callCount(), 0);
});

for (const [name, response, failure = transportFailure] of [
  ['redirect', () => new Response(null, { status: 307, headers: { location: 'https://other.test/' } })],
  ['encoded response', () => new Response('fixture receipt', { headers: { 'content-encoding': 'gzip' } })],
  ['oversized response', () => new Response(new Uint8Array(16 * 1024 + 1))],
  ['receipt without a version', async (request) => new Response(completionReceipt(await request.text())),
    'Unconfirmed(Some(CompletionVersion))'],
  ['embedded error', () => new Response('<Error/>', { headers: { 'x-amz-version-id': version } }),
    'Unconfirmed(Some(EmbeddedError))'],
]) {
  test(`refuses ${name} after one fetch`, async (t) => {
    const fetch = t.mock.method(globalThis, 'fetch', async (request) => response(request));
    await assert.rejects(complete('success'), rejectedWith(failure));
    assert.equal(fetch.mock.callCount(), 1);
  });
}

test('the single deadline covers body reads and aborts the fetch', async (t) => {
  let signal;
  const fetch = t.mock.method(globalThis, 'fetch', async (request) => {
    signal = request.signal;
    return new Response(new ReadableStream());
  });
  await assert.rejects(complete('deadline'), rejectedWith(transportFailure));
  assert.equal(fetch.mock.callCount(), 1);
  assert.equal(signal.aborted, true);
});
