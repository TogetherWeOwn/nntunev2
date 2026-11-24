'use strict';

const assert = require('node:assert/strict');
const { createHash, createHmac } = require('node:crypto');
const { test } = require('node:test');
const { s3_completion_test: complete } = require('./ingest_wasm.cjs');

const version = 'fixture-version';
const unconfirmed = (error) => error === 'completion send unconfirmed';
const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');
const mac = (key, text) => createHmac('sha256', key).update(text).digest();

test('signs the exact multipart POST once and collects its version and body', async (t) => {
  const fetch = t.mock.method(globalThis, 'fetch', async (request) => {
    const url = new URL(request.url);
    const path = '/fixture-bucket/media/abababababab40ab80ababababababab/cdcdcdcdcdcd40cd80cdcdcdcdcdcdcd';
    const query = 'uploadId=fixture%2B%2F%3D%3F';
    assert.equal(url.href, `https://fixture.test:8443${path}?${query}`);
    assert.equal(request.method, 'POST');
    assert.equal(request.redirect, 'manual');
    assert.equal(request.cache, 'no-store');
    assert.equal(request.headers.get('accept-encoding'), 'identity');
    const body = await request.text();
    assert.equal(body, '<CompleteMultipartUpload/>');
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
    return new Response('fixture receipt', { headers: { 'x-amz-version-id': version } });
  });
  assert.equal(await complete('success'), version);
  assert.equal(fetch.mock.callCount(), 1);
});

test('refuses a foreign configuration or expired deadline before fetch', async (t) => {
  const fetch = t.mock.method(globalThis, 'fetch', () => { throw new Error('unexpected fetch'); });
  await assert.rejects(complete('foreign'), unconfirmed);
  await assert.rejects(complete('expired'), unconfirmed);
  assert.equal(fetch.mock.callCount(), 0);
});

for (const [name, response] of [
  ['redirect', () => new Response(null, { status: 307, headers: { location: 'https://other.test/' } })],
  ['encoded response', () => new Response('fixture receipt', { headers: { 'content-encoding': 'gzip' } })],
  ['oversized response', () => new Response(new Uint8Array(16 * 1024 + 1))],
]) {
  test(`refuses ${name} after one fetch`, async (t) => {
    const fetch = t.mock.method(globalThis, 'fetch', async () => response());
    await assert.rejects(complete('success'), unconfirmed);
    assert.equal(fetch.mock.callCount(), 1);
  });
}

test('the single deadline covers body reads and aborts the fetch', async (t) => {
  let signal;
  const fetch = t.mock.method(globalThis, 'fetch', async (request) => {
    signal = request.signal;
    return new Response(new ReadableStream());
  });
  await assert.rejects(complete('deadline'), unconfirmed);
  assert.equal(fetch.mock.callCount(), 1);
  assert.equal(signal.aborted, true);
});
