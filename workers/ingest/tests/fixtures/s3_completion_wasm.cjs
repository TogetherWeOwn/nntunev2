'use strict';

const assert = require('node:assert/strict');
const { createHash, createHmac } = require('node:crypto');
const { test } = require('node:test');
const { s3_completion_test: complete } = require('./ingest_wasm.cjs');

const version = 'fixture-version+/=?';
const versionQuery = 'versionId=fixture-version%2B%2F%3D%3F';
const ciphertextLength = 48 + 1 + 16;
const headHeaders = { 'x-amz-version-id': version, 'content-length': String(ciphertextLength) };
const bucket = 'fixture-bucket';
const mediaKey = 'media/abababababab40ab80ababababababab/cdcdcdcdcdcd40cd80cdcdcdcdcdcdcd';
const mapId = 'efefefefefef40ef80efefefefefefef';
const sidecarKey = `media-digests/${mediaKey.slice('media/'.length)}/${mapId}/00000000`;
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

async function verifySignedRequest(request, method, query, operationHeaders, objectKey = mediaKey) {
  const path = `/${bucket}/${objectKey}`;
  assert.equal(request.url, `https://fixture.test:8443${path}${query ? `?${query}` : ''}`);
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
    assert.equal(body.length, ciphertextLength);
    return new Response(null, { headers: { etag: '"fixture-part"', 'x-amz-checksum-sha256': checksum(body) } });
  });
  assert.equal(await complete('part'), '"fixture-part"');
  assert.equal(fetch.mock.callCount(), 1);
});

test('signs the sealer-derived sidecar page and captures its returned version', async (t) => {
  let sentPage;
  const fetch = t.mock.method(globalThis, 'fetch', async (request) => {
    sentPage = await verifySignedRequest(request, 'PUT', '', undefined, sidecarKey);
    return new Response(null, { headers: { 'x-amz-version-id': version } });
  });
  const receipt = JSON.parse(await complete('sidecar'));
  assert.equal(fetch.mock.callCount(), 1);
  assert.deepEqual(sentPage, Buffer.from(receipt.expectedPage, 'hex'));
  assert.equal(sentPage.length, 124);
  assert.equal(sentPage.subarray(0, 8).toString(), 'NNTDGP01');
  assert.equal(sentPage.subarray(12, 28).toString('hex'), mapId);
  assert.equal(sentPage.readUInt32BE(80), 0);
  assert.equal(receipt.version, version);
});

test('refuses sidecar PUT for a different provider or bucket or an expired deadline', async (t) => {
  const fetch = t.mock.method(globalThis, 'fetch', () => { throw new Error('unexpected fetch'); });
  for (const mode of ['sidecar-foreign', 'sidecar-bucket', 'sidecar-expired']) {
    await assert.rejects(complete(mode), rejectedWith('Transport'));
  }
  assert.equal(fetch.mock.callCount(), 0);
});

for (const [name, response, failure] of [
  ['HTTP 500', () => new Response(null, { status: 500, headers: { 'x-amz-version-id': version } }), 'Receipt(Response)'],
  ['non-empty body', () => new Response('unexpected', { headers: { 'x-amz-version-id': version } }), 'Receipt(Response)'],
  ['missing version', () => new Response(null), 'Receipt(Version)'],
  ['null version', () => new Response(null, { headers: { 'x-amz-version-id': 'null' } }), 'Receipt(Version)'],
  ['joined versions', () => new Response(null, { headers: { 'x-amz-version-id': `${version}, other` } }), 'Receipt(Version)'],
  ['redirect', () => new Response(null, { status: 307, headers: { location: 'https://other.test/' } }), 'Transport'],
]) {
  test(`refuses ${name} in a sidecar PUT reply without resending`, async (t) => {
    const fetch = t.mock.method(globalThis, 'fetch', async (request) => {
      await verifySignedRequest(request, 'PUT', '', undefined, sidecarKey);
      return response();
    });
    await assert.rejects(complete('sidecar'), rejectedWith(failure));
    assert.equal(fetch.mock.callCount(), 1);
  });
}

test('uses the initialized upload ID, signed part receipt and completed version through HEAD', async (t) => {
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
      assert.equal(body.length, ciphertextLength);
      uploadedChecksum = checksum(body);
      return new Response(null, { headers: { etag: '"fixture-part"', 'x-amz-checksum-sha256': uploadedChecksum } });
    }
    if (request.method === 'HEAD') {
      assert.equal(request.body, null);
      const body = await verifySignedRequest(request, 'HEAD', versionQuery, {});
      assert.equal(body.length, 0);
      return new Response(null, { headers: headHeaders });
    }
    const manifest = (await verifySignedRequest(request, 'POST', initiatedQuery)).toString();
    assert.ok(manifest.includes(`<ChecksumSHA256>${uploadedChecksum}</ChecksumSHA256>`));
    return new Response(completionReceipt(manifest, 'fixture-part'), { headers: { 'x-amz-version-id': version } });
  });
  assert.equal(await complete('init-upload-completion-head'), version);
  assert.deepEqual(requests, [
    'POST uploads=', `PUT partNumber=1&${initiatedQuery}`, `POST ${initiatedQuery}`, `HEAD ${versionQuery}`,
  ]);
});

const readbackPageVersion = 'page+%2F/=';
const readbackPageQuery = 'versionId=page%2B%252F%2F%3D';
const readbackHeaders = (bytes, id) => ({
  'content-length': String(bytes.length), 'x-amz-version-id': id,
});

function mockReadback(t, { mediaReply, pageReply, waitForPage = false } = {}) {
  let media;
  let page;
  let endMedia;
  const gets = [];
  t.mock.method(globalThis, 'fetch', async (request) => {
    const url = new URL(request.url);
    if (url.search === '?uploads=') {
      await verifySignedRequest(request, 'POST', 'uploads=', initiationHeaders);
      return new Response(initiationReceipt());
    }
    if (request.method === 'PUT' && url.search) {
      media = await verifySignedRequest(request, 'PUT', `partNumber=1&${initiatedQuery}`);
      return new Response(null, { headers: { etag: '"fixture-part"', 'x-amz-checksum-sha256': checksum(media) } });
    }
    if (request.method === 'POST') {
      const manifest = (await verifySignedRequest(request, 'POST', initiatedQuery)).toString();
      return new Response(completionReceipt(manifest, 'fixture-part'), { headers: { 'x-amz-version-id': version } });
    }
    if (request.method === 'HEAD') {
      await verifySignedRequest(request, 'HEAD', versionQuery, {});
      return new Response(null, { headers: readbackHeaders(media, version) });
    }
    if (request.method === 'PUT') {
      page = await verifySignedRequest(request, 'PUT', '', undefined, sidecarKey);
      return new Response(null, { headers: { 'x-amz-version-id': readbackPageVersion } });
    }
    assert.equal(request.body, null);
    assert.equal(request.headers.has('range'), false);
    if (url.pathname === `/${bucket}/${mediaKey}`) {
      await verifySignedRequest(request, 'GET', versionQuery, {});
      gets.push(mediaKey);
      const headers = readbackHeaders(media, version);
      if (mediaReply) return mediaReply(media, headers, request);
      const body = new ReadableStream({
        start(controller) {
          controller.enqueue(new Uint8Array());
          controller.enqueue(media.subarray(0, 7));
          controller.enqueue(media.subarray(7));
          if (waitForPage) endMedia = () => controller.close();
          else controller.close();
        },
      });
      return new Response(body, { headers });
    }
    await verifySignedRequest(request, 'GET', readbackPageQuery, {}, sidecarKey);
    gets.push(sidecarKey);
    endMedia?.();
    const headers = readbackHeaders(page, readbackPageVersion);
    return pageReply ? pageReply(page, headers) : new Response(page, { headers });
  });
  return gets;
}

test('streams the completed media and sidecar versions into authenticated readback', async (t) => {
  // The media stream cannot finish until the verifier opens the sidecar.
  const gets = mockReadback(t, { waitForPage: true });
  assert.equal(await complete('readback'), version);
  assert.deepEqual(gets, [mediaKey, sidecarKey]);
});

for (const [name, mediaReply, failure] of [
  ['HTTP 206', (bytes, headers) => new Response(bytes, { status: 206, headers }), 'Response'],
  ['wrong version', (bytes, headers) => new Response(bytes, {
    headers: { ...headers, 'x-amz-version-id': 'other' },
  }), 'Response'],
  ['missing version', (bytes, headers) => new Response(bytes, {
    headers: { 'content-length': headers['content-length'] },
  }), 'Response'],
  ['joined versions', (bytes, headers) => new Response(bytes, {
    headers: { ...headers, 'x-amz-version-id': `${version}, other` },
  }), 'Transport'],
  ['wrong length', (bytes, headers) => new Response(bytes, {
    headers: { ...headers, 'content-length': String(bytes.length + 1) },
  }), 'Response'],
  ['joined lengths', (bytes, headers) => new Response(bytes, {
    headers: { ...headers, 'content-length': `${bytes.length}, ${bytes.length}` },
  }), 'Transport'],
  ['content range', (bytes, headers) => new Response(bytes, {
    headers: { ...headers, 'content-range': `bytes 0-${bytes.length - 1}/${bytes.length}` },
  }), 'Response'],
  ['content encoding', (bytes, headers) => new Response(bytes, {
    headers: { ...headers, 'content-encoding': 'gzip' },
  }), 'Transport'],
  ['redirect', (_bytes, headers) => new Response(null, {
    status: 307, headers: { ...headers, location: 'https://other.test/' },
  }), 'Transport'],
]) {
  test(`refuses ${name} on the first readback GET`, async (t) => {
    const gets = mockReadback(t, { mediaReply });
    await assert.rejects(complete('readback'), rejectedWith(`Body(${failure})`));
    assert.deepEqual(gets, [mediaKey]);
  });
}

test('rejects a truncated sidecar and trailing media even with exact declared lengths', async (t) => {
  await t.test('truncated sidecar', async (t) => {
    const gets = mockReadback(t, {
      pageReply: (bytes, headers) => new Response(bytes.subarray(0, -1), { headers }),
    });
    await assert.rejects(complete('readback'), rejectedWith('Body(Truncated)'));
    assert.deepEqual(gets, [mediaKey, sidecarKey]);
  });
  await t.test('trailing media', async (t) => {
    const gets = mockReadback(t, {
      mediaReply: (bytes, headers) => new Response(Buffer.concat([bytes, Buffer.of(0)]), { headers }),
    });
    await assert.rejects(complete('readback'), rejectedWith('Body(Trailing)'));
    assert.deepEqual(gets, [mediaKey, sidecarKey]);
  });
});

test('rejects changed sidecar or ciphertext bytes with valid version and length headers', async (t) => {
  const changed = (bytes, headers) => {
    const body = Buffer.from(bytes);
    body[body.length - 1] ^= 1;
    return new Response(body, { headers });
  };
  await t.test('changed sidecar', async (t) => {
    const gets = mockReadback(t, { pageReply: changed });
    await assert.rejects(complete('readback'), rejectedWith('Body(Content(Frame(Page(HashMismatch))))'));
    assert.deepEqual(gets, [mediaKey, sidecarKey]);
  });
  await t.test('changed ciphertext', async (t) => {
    const gets = mockReadback(t, { mediaReply: changed });
    await assert.rejects(complete('readback'), rejectedWith('Body(Content(Frame(DigestMismatch)))'));
    assert.deepEqual(gets, [mediaKey, sidecarKey]);
  });
});

test('aborts readback when the media never reaches EOF', async (t) => {
  let signal;
  const gets = mockReadback(t, {
    mediaReply(bytes, headers, request) {
      signal = request.signal;
      const body = new ReadableStream({ start(controller) { controller.enqueue(bytes); } });
      return new Response(body, { headers });
    },
  });
  await assert.rejects(complete('readback-deadline'), rejectedWith('Body(Transport)'));
  assert.deepEqual(gets, [mediaKey, sidecarKey]);
  assert.equal(signal.aborted, true);
});

test('refuses a foreign completion context before sending HEAD', async (t) => {
  const fetch = t.mock.method(globalThis, 'fetch', async (request) => {
    const body = await verifySignedRequest(request, 'POST', uploadQuery);
    return new Response(completionReceipt(body.toString()), { headers: { 'x-amz-version-id': version } });
  });
  await assert.rejects(complete('head-context'), rejectedWith('InvalidRequest'));
  assert.equal(fetch.mock.callCount(), 1);
});

for (const [name, response, failure = 'InvalidResponse'] of [
  ['HTTP 500', () => new Response(null, { status: 500, headers: headHeaders })],
  ['non-empty body', () => new Response('unexpected', { headers: headHeaders })],
  ['content range', () => new Response(null, { headers: { ...headHeaders, 'content-range': 'bytes 0-64/65' } })],
  ['encoded response', () => new Response(null, { headers: { ...headHeaders, 'content-encoding': 'gzip' } }), 'Transport'],
  ['missing version', () => new Response(null, { headers: { 'content-length': headHeaders['content-length'] } }),
    'Receipt(MissingMetadata)'],
  ['wrong version', () => new Response(null, { headers: { ...headHeaders, 'x-amz-version-id': 'other' } }),
    'Receipt(WrongObject)'],
  ['null version', () => new Response(null, { headers: { ...headHeaders, 'x-amz-version-id': 'null' } })],
  ['joined versions', () => new Response(null, { headers: { ...headHeaders, 'x-amz-version-id': `${version}, other` } })],
  ['missing length', () => new Response(null, { headers: { 'x-amz-version-id': version } }), 'Receipt(MissingMetadata)'],
  ['wrong length', () => new Response(null, { headers: { ...headHeaders, 'content-length': String(ciphertextLength + 1) } }),
    'Receipt(SizeMismatch)'],
  ['signed length', () => new Response(null, { headers: { ...headHeaders, 'content-length': `+${ciphertextLength}` } })],
  ['joined lengths', () => new Response(null, {
    headers: { ...headHeaders, 'content-length': `${ciphertextLength}, ${ciphertextLength}` },
  })],
]) {
  test(`refuses ${name} in a HEAD reply without resending`, async (t) => {
    const fetch = t.mock.method(globalThis, 'fetch', async (request) => {
      if (request.method === 'HEAD') {
        await verifySignedRequest(request, 'HEAD', versionQuery, {});
        return response();
      }
      const body = await verifySignedRequest(request, 'POST', uploadQuery);
      return new Response(completionReceipt(body.toString()), { headers: { 'x-amz-version-id': version } });
    });
    await assert.rejects(complete('head'), rejectedWith(failure));
    assert.equal(fetch.mock.callCount(), 2);
  });
}

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
  ['missing bucket', initiationReceipt().replace(`<Bucket>${bucket}</Bucket>`, '')],
  ['missing key', initiationReceipt().replace(`<Key>${mediaKey}</Key>`, '')],
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
