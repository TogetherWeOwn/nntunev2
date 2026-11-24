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
const checksum = (bytes) => createHash('sha256').update(bytes).digest('base64');
const mac = (key, text) => createHmac('sha256', key).update(text).digest();
const uploadQuery = 'uploadId=fixture%2B%2F%3D%3F';
const initiatedId = 'new+/=?';
const initiatedQuery = 'uploadId=new%2B%2F%3D%3F';
const initiationHeaders = { 'x-amz-checksum-algorithm': 'SHA256', 'x-amz-checksum-type': 'COMPOSITE' };

function initiationReceipt(observedBucket = bucket, observedKey = mediaKey, uploadId = initiatedId) {
  return '<?xml version="1.0" encoding="UTF-8"?>'
    + '<InitiateMultipartUploadResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">'
    + `<Bucket>${observedBucket}</Bucket><Key>${observedKey}</Key><UploadId>${uploadId}</UploadId>`
    + '</InitiateMultipartUploadResult>';
}

function completionReceipt(manifest, etag = 'fixture-etag') {
  const prefix = '<CompleteMultipartUpload xmlns="http://s3.amazonaws.com/doc/2006-03-01/">'
    + `<Part><PartNumber>1</PartNumber><ETag>&quot;${etag}&quot;</ETag><ChecksumSHA256>`;
  const suffix = '</ChecksumSHA256></Part></CompleteMultipartUpload>';
  assert.ok(manifest.startsWith(prefix));
  assert.ok(manifest.endsWith(suffix));
  const part = manifest.slice(prefix.length, -suffix.length);
  assert.match(part, /^[A-Za-z0-9+/]{43}=$/);
  const composite = checksum(Buffer.from(part, 'base64'));
  return `<CompleteMultipartUploadResult><Bucket>${bucket}</Bucket><Key>${mediaKey}</Key>`
    + `<ChecksumSHA256>${composite}-1</ChecksumSHA256></CompleteMultipartUploadResult>`;
}

async function verifySignedRequest(request, method, query, operationHeaders) {
  const path = `/${bucket}/${mediaKey}`;
  assert.equal(request.url, `https://fixture.test:8443${path}?${query}`);
  assert.equal(request.method, method);
  assert.equal(request.redirect, 'manual');
  assert.equal(request.cache, 'no-store');
  assert.equal(request.headers.get('accept-encoding'), 'identity');
  const body = Buffer.from(await request.arrayBuffer());
  const payload = sha256(body);
  const date = request.headers.get('x-amz-date');
  assert.match(date, /^\d{8}T\d{6}Z$/);
  const fields = {
    host: 'fixture.test:8443',
    'x-amz-content-sha256': payload,
    'x-amz-date': date,
    ...(operationHeaders ?? (method === 'PUT'
      ? { 'x-amz-checksum-sha256': checksum(body) } : { 'content-type': 'application/xml' })),
  };
  const names = Object.keys(fields).sort();
  for (const name of names.filter((value) => value !== 'host')) {
    assert.equal(request.headers.get(name), fields[name]);
  }
  const signedHeaders = names.join(';');
  const canonical = [method, path, query, ...names.map((name) => `${name}:${fields[name]}`), '', signedHeaders, payload].join('\n');
  const day = date.slice(0, 8);
  const scope = `${day}/fixture-region/s3/aws4_request`;
  const key = [day, 'fixture-region', 's3', 'aws4_request'].reduce(mac, 'AWS4fixture-secret');
  const signature = mac(key, `AWS4-HMAC-SHA256\n${date}\n${scope}\n${sha256(canonical)}`).toString('hex');
  assert.equal(request.headers.get('authorization'),
    `AWS4-HMAC-SHA256 Credential=FIXTUREACCESS/${scope}, SignedHeaders=${signedHeaders}, Signature=${signature}`);
  return body;
}

test('signs the exact multipart POST once and collects its version and body', async (t) => {
  const fetch = t.mock.method(globalThis, 'fetch', async (request) => {
    const body = await verifySignedRequest(request, 'POST', uploadQuery);
    return new Response(completionReceipt(body.toString()), { headers: { 'x-amz-version-id': version } });
  });
  assert.equal(await complete('success'), version);
  assert.equal(fetch.mock.callCount(), 1);
});

test('signs the ciphertext part and accepts only its matching checksum and ETag', async (t) => {
  const fetch = t.mock.method(globalThis, 'fetch', async (request) => {
    const body = await verifySignedRequest(request, 'PUT', `partNumber=1&${uploadQuery}`);
    assert.equal(body.length, 48 + 1 + 16);
    return new Response(null, { headers: { etag: '"fixture-part"', 'x-amz-checksum-sha256': checksum(body) } });
  });
  assert.equal(await complete('part'), '"fixture-part"');
  assert.equal(fetch.mock.callCount(), 1);
});

test('uses the initialized upload ID and signed part receipt in completion', async (t) => {
  const requests = [];
  let uploadedChecksum;
  t.mock.method(globalThis, 'fetch', async (request) => {
    const query = new URL(request.url).search.slice(1);
    requests.push(`${request.method} ${query}`);
    if (query === 'uploads=') {
      const body = await verifySignedRequest(request, 'POST', 'uploads=', initiationHeaders);
      assert.equal(body.length, 0);
      return new Response(initiationReceipt());
    }
    if (request.method === 'PUT') {
      const body = await verifySignedRequest(request, 'PUT', `partNumber=1&${initiatedQuery}`);
      uploadedChecksum = checksum(body);
      return new Response(null, { headers: { etag: '"fixture-part"', 'x-amz-checksum-sha256': uploadedChecksum } });
    }
    const manifest = (await verifySignedRequest(request, 'POST', initiatedQuery)).toString();
    assert.ok(manifest.includes(`<ChecksumSHA256>${uploadedChecksum}</ChecksumSHA256>`));
    return new Response(completionReceipt(manifest, 'fixture-part'), { headers: { 'x-amz-version-id': version } });
  });
  assert.equal(await complete('init-upload-completion'), version);
  assert.deepEqual(requests, ['POST uploads=', `PUT partNumber=1&${initiatedQuery}`, `POST ${initiatedQuery}`]);
});

test('refuses initiation with an unrelated provider or key before fetch', async (t) => {
  const fetch = t.mock.method(globalThis, 'fetch', () => { throw new Error('unexpected fetch'); });
  await assert.rejects(complete('init-foreign'), rejectedWith(transportFailure));
  await assert.rejects(complete('init-other'), rejectedWith('InvalidRequest'));
  assert.equal(fetch.mock.callCount(), 0);
});

for (const [name, body, status = 200] of [
  ['HTTP 500', initiationReceipt(), 500],
  ['wrong bucket', initiationReceipt('other-bucket')],
  ['wrong key', initiationReceipt(bucket, 'media/other')],
  ['empty upload ID', initiationReceipt(bucket, mediaKey, '')],
  ['control character', initiationReceipt(bucket, mediaKey, 'upload&#10;id')],
  ['duplicate upload ID', initiationReceipt(bucket, mediaKey, 'one</UploadId><UploadId>two')],
  ['DOCTYPE', initiationReceipt().replace('?>', '?><!DOCTYPE InitiateMultipartUploadResult>')],
  ['embedded error', '<Error/>'],
]) {
  test(`refuses ${name} in an initiation reply without restarting the upload`, async (t) => {
    const fetch = t.mock.method(globalThis, 'fetch', async () => new Response(body, { status }));
    await assert.rejects(complete('init'), rejectedWith('Unconfirmed(Some(InitiationReceipt))'));
    assert.equal(fetch.mock.callCount(), 1);
  });
}

test('refuses an unrelated checkpoint or provider before uploading the part', async (t) => {
  const fetch = t.mock.method(globalThis, 'fetch', () => { throw new Error('unexpected fetch'); });
  for (const mode of ['part-other', 'part-context']) {
    await assert.rejects(complete(mode), rejectedWith('InvalidRequest'));
  }
  await assert.rejects(complete('part-foreign'), rejectedWith(transportFailure));
  assert.equal(fetch.mock.callCount(), 0);
});

for (const [name, response, error = 'PartReceipt'] of [
  ['HTTP 500', (headers) => new Response(null, { status: 500, headers })],
  ['non-empty success body', (headers) => new Response('unexpected', { headers })],
  ['missing checksum', () => new Response(null, { headers: { etag: '"fixture-part"' } })],
  ['wrong checksum', (headers) => new Response(null, { headers: { ...headers, 'x-amz-checksum-sha256': 'wrong' } }), 'Checksum'],
  ['missing ETag', ({ etag: _, ...headers }) => new Response(null, { headers })],
  ['joined ETags', (headers) => new Response(null, { headers: { ...headers, etag: '"first", "second"' } })],
]) {
  test(`rejects ${name} from UploadPart without resending`, async (t) => {
    const fetch = t.mock.method(globalThis, 'fetch', async (request) => response({
      etag: '"fixture-part"',
      'x-amz-checksum-sha256': checksum(Buffer.from(await request.arrayBuffer())),
    }));
    await assert.rejects(complete('part'), rejectedWith(`Unconfirmed(Some(${error}))`));
    assert.equal(fetch.mock.callCount(), 1);
  });
}

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
