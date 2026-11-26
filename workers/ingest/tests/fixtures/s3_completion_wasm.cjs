'use strict';

const assert = require('node:assert/strict');
const { test } = require('node:test');
const { s3_completion_test: complete, s3_fixture_retirement_test: retire } = require('./ingest_wasm.cjs');
const {
  authorization, bucket, checksum, completionMarker, completionReceipt, initiatedId, initiatedQuery,
  initiationHeaders, initiationReceipt, mapId, mediaKey, sha256, sidecarKey, uploadQuery,
} = require('./s3_fixture_protocol.cjs');

const version = 'fixture-version+/=?';
const versionQuery = 'versionId=fixture-version%2B%2F%3D%3F';
const wireHeaderLength = 48;
const ciphertextLength = wireHeaderLength + 1 + 16;
const headHeaders = { 'x-amz-version-id': version, 'content-length': String(ciphertextLength) };
const transportFailure = 'Unconfirmed(None)';
const rejectedWith = (expected) => (error) => error === expected;

async function verifySignedRequest(request, method, query, operationHeaders, objectKey = mediaKey, targetBucket = bucket) {
  const path = `/${targetBucket}/${objectKey}`;
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
  assert.equal(request.headers.get('authorization'),
    authorization(method, path, query, fields, 'FIXTUREACCESS', 'fixture-secret', 'fixture-region'));
  return body;
}

const retirementBucket = 'nntunev2-private-fixtures-staging';

for (const key of [mediaKey, sidecarKey]) {
  test(`retirement signs DELETE and HEAD against the same exact version of ${key.split('/')[0]}`, async (t) => {
    const methods = [];
    const fetch = t.mock.method(globalThis, 'fetch', async (request) => {
      methods.push(request.method);
      const body = await verifySignedRequest(request, request.method, versionQuery, {}, key, retirementBucket);
      assert.equal(body.length, 0);
      return new Response(null, { status: request.method === 'DELETE' ? 204 : 404 });
    });
    await retire(retirementBucket, key, version);
    assert.deepEqual(methods, ['DELETE', 'HEAD']);
    assert.equal(fetch.mock.callCount(), 2);
  });
}

test('retirement refuses a foreign bucket, unexpected key or absent version before fetch', async (t) => {
  const fetch = t.mock.method(globalThis, 'fetch', () => assert.fail('unexpected fetch'));
  for (const args of [[bucket, mediaKey, version], [retirementBucket, `${mediaKey}/../other`, version],
    [retirementBucket, sidecarKey.replace(/00000000$/, '00000001'), version], [retirementBucket, mediaKey, 'null']]) {
    await assert.rejects(retire(...args), rejectedWith('Refused'));
  }
  assert.equal(fetch.mock.callCount(), 0);
});

test('retirement refuses to confirm when the exact version still answers HEAD', async (t) => {
  t.mock.method(globalThis, 'fetch', async (request) => {
    await verifySignedRequest(request, request.method, versionQuery, {}, mediaKey, retirementBucket);
    return new Response(null, { status: request.method === 'DELETE' ? 204 : 200 });
  });
  await assert.rejects(retire(retirementBucket, mediaKey, version), rejectedWith('Unconfirmed'));
});

test('signs the exact multipart POST once and collects its version and body', async (t) => {
  const fetch = t.mock.method(globalThis, 'fetch', async (request) => {
    const body = await verifySignedRequest(request, 'POST', uploadQuery);
    return new Response(completionReceipt(body.toString()), { headers: { 'x-amz-version-id': version } });
  });
  assert.equal(await complete('success'), version);
  assert.equal(fetch.mock.callCount(), 1);
});

for (const [scenario, overriddenHeaders, failure] of [
  ['matching HEAD', {}, null],
  ['stalled response body', {}, null],
  ['completion still in flight', {}, transportFailure],
  ['foreign initiation', { [completionMarker]: 'different-attempt' }, transportFailure],
  ['different checksum', { 'x-amz-checksum-sha256': 'different-checksum' }, transportFailure],
  ['wrong length', { 'content-length': String(ciphertextLength + 1) }, transportFailure],
  ['null version', { 'x-amz-version-id': 'null' }, 'Unconfirmed(Some(CompletionVersion))'],
]) {
  test(`recovers a lost completion response with ${scenario}`, async (t) => {
    const stillCommitting = scenario === 'completion still in flight';
    const stalled = stillCommitting || scenario === 'stalled response body';
    const operations = [];
    let marker;
    let partChecksum;
    let committed;
    let stalledSignal;
    let recoveryResponse;
    t.mock.method(globalThis, 'fetch', async (request) => {
      const query = new URL(request.url).search.slice(1);
      operations.push(`${request.method} ${query}`);
      if (request.method === 'POST' && query === 'uploads=') {
        await verifySignedRequest(request, 'POST', 'uploads=', initiationHeaders);
        marker = request.headers.get(completionMarker);
        return new Response(initiationReceipt());
      }
      if (request.method === 'PUT') {
        const body = await verifySignedRequest(request, 'PUT', `partNumber=1&${initiatedQuery}`);
        partChecksum = checksum(body);
        return new Response(null, {
          headers: { etag: '"fixture-part"', 'x-amz-checksum-sha256': partChecksum },
        });
      }
      if (request.method === 'POST') {
        assert.equal(committed, undefined, 'completion was sent twice');
        const manifest = (await verifySignedRequest(request, 'POST', initiatedQuery)).toString();
        const receipt = completionReceipt(manifest, 'fixture-part');
        assert.ok(manifest.includes(`<ChecksumSHA256>${partChecksum}</ChecksumSHA256>`));
        if (!stillCommitting) {
          committed = {
            ...headHeaders,
            [completionMarker]: marker,
            'x-amz-checksum-type': 'COMPOSITE',
            'x-amz-checksum-sha256': /<ChecksumSHA256>([^<]+)/.exec(receipt)[1],
          };
        }
        if (stalled) {
          stalledSignal = request.signal;
          return new Response(new ReadableStream(), {
            headers: committed ? { 'x-amz-version-id': version } : {},
          });
        }
        throw new Error('completion response lost after storing the version');
      }
      if (query === '') {
        if (stalled) assert.equal(stalledSignal?.aborted, true);
        await verifySignedRequest(request, 'HEAD', '', { 'x-amz-checksum-mode': 'ENABLED' });
        recoveryResponse = stillCommitting
          ? new Response(null, { status: 404 })
          : new Response(null, { headers: { ...committed, ...overriddenHeaders } });
        return recoveryResponse;
      }
      await verifySignedRequest(request, 'HEAD', versionQuery, {});
      return new Response(null, { headers: committed });
    });
    const result = complete('init-completion-response-lost');
    if (failure) {
      await assert.rejects(result, rejectedWith(failure));
    } else {
      assert.equal(await result, version);
    }
    assert.equal(Boolean(committed), !stillCommitting);
    assert.equal(recoveryResponse?.status, stillCommitting ? 404 : 200);
    assert.deepEqual(operations, [
      'POST uploads=', `PUT partNumber=1&${initiatedQuery}`, `POST ${initiatedQuery}`, 'HEAD ',
      ...(failure ? [] : [`HEAD ${versionQuery}`]),
    ]);
  });
}

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

function multipartManifest(parts) {
  const entries = [...parts].map(([number, receipt]) =>
    `<Part><PartNumber>${number}</PartNumber><ETag>&quot;${receipt.etag.slice(1, -1)}&quot;</ETag>`
    + `<ChecksumSHA256>${receipt['x-amz-checksum-sha256']}</ChecksumSHA256></Part>`);
  return '<CompleteMultipartUpload xmlns="http://s3.amazonaws.com/doc/2006-03-01/">'
    + entries.join('') + '</CompleteMultipartUpload>';
}

function multipartCompletionReceipt(parts) {
  const composite = checksum(Buffer.concat([...parts.values()].map(
    (receipt) => Buffer.from(receipt['x-amz-checksum-sha256'], 'base64'))));
  return new Response(
    `<CompleteMultipartUploadResult><Bucket>${bucket}</Bucket><Key>${mediaKey}</Key>`
    + `<ChecksumSHA256>${composite}-${parts.size}</ChecksumSHA256></CompleteMultipartUploadResult>`,
    { headers: { 'x-amz-version-id': version } },
  );
}

test('resumes multipart upload with only the missing part and preserves the confirmed receipt', async (t) => {
  const partBytes = 5_244_160;
  const tailBytes = wireHeaderLength + Math.ceil(partBytes / 65_536) * 16;
  const requests = [];
  const stored = new Map();
  let interrupted;
  t.mock.method(globalThis, 'fetch', async (request) => {
    const url = new URL(request.url);
    const query = url.search.slice(1);
    requests.push(`${request.method} ${query}`);
    if (request.method === 'PUT') {
      const number = Number(url.searchParams.get('partNumber'));
      assert.ok(number === 1 || number === 2);
      assert.equal(stored.has(number), false, 'a confirmed part must never be overwritten');
      const body = await verifySignedRequest(request, 'PUT', `partNumber=${number}&${uploadQuery}`);
      assert.equal(body.length, number === 1 ? partBytes : tailBytes);
      if (number === 2 && !interrupted) {
        assert.deepEqual([...stored.keys()], [1]);
        interrupted = body;
        throw new Error('synthetic interruption before storing part 2');
      }
      if (number === 2) {
        assert.deepEqual(body, interrupted);
      }
      const receipt = { etag: `"fixture-part-${number}"`, 'x-amz-checksum-sha256': checksum(body) };
      stored.set(number, receipt);
      return new Response(null, { headers: receipt });
    }
    const manifest = (await verifySignedRequest(request, 'POST', uploadQuery)).toString();
    assert.deepEqual([...stored.keys()], [1, 2]);
    assert.equal(manifest, multipartManifest(stored));
    return multipartCompletionReceipt(stored);
  });
  assert.equal(await complete('multipart-resume'), version);
  assert.deepEqual(requests, [
    `PUT partNumber=1&${uploadQuery}`, `PUT partNumber=2&${uploadQuery}`,
    `PUT partNumber=2&${uploadQuery}`, `POST ${uploadQuery}`,
  ]);
});

for (const [name, error] of [
  ['completes a matching full multipart inventory', null],
  ['keeps completion pending when the provider refuses part 2', 'CompletionStatus'],
  ['keeps completion pending when the returned composite disagrees with part 2', 'Checksum'],
]) {
  test(name, async (t) => {
    const requests = [];
    const confirmed = new Map();
    const stored = new Map();
    let secondPart;
    t.mock.method(globalThis, 'fetch', async (request) => {
      const url = new URL(request.url);
      requests.push(`${request.method} ${url.search.slice(1)}`);
      if (request.method === 'PUT') {
        const number = confirmed.size + 1;
        assert.ok(number <= 2);
        const body = await verifySignedRequest(request, 'PUT', `partNumber=${number}&${uploadQuery}`);
        const receipt = { etag: `"fixture-part-${number}"`, 'x-amz-checksum-sha256': checksum(body) };
        confirmed.set(number, receipt);
        stored.set(number, receipt);
        if (number === 2) {
          secondPart = body;
        }
        return new Response(null, { headers: receipt });
      }

      const manifest = (await verifySignedRequest(request, 'POST', uploadQuery)).toString();
      assert.deepEqual([...confirmed.keys()], [1, 2]);
      assert.equal(manifest, multipartManifest(confirmed));
      if (error) {
        // Replace part 2 after confirmation with different bytes of the same length.
        secondPart[0] ^= 1;
        stored.set(2, { etag: '"replacement-part-2"', 'x-amz-checksum-sha256': checksum(secondPart) });
      }
      assert.deepEqual([...stored.keys()], [1, 2]);
      assert.deepEqual(stored.get(1), confirmed.get(1));
      if (error) {
        assert.notEqual(stored.get(2).etag, confirmed.get(2).etag);
        assert.notEqual(stored.get(2)['x-amz-checksum-sha256'], confirmed.get(2)['x-amz-checksum-sha256']);
        if (error === 'CompletionStatus') {
          return new Response('<Error><Code>InvalidPart</Code></Error>', { status: 400 });
        }
      } else {
        assert.equal(manifest, multipartManifest(stored));
      }
      return multipartCompletionReceipt(stored);
    });

    if (error) {
      await assert.rejects(complete('multipart-resume-full'), rejectedWith(`Unconfirmed(Some(${error}))`));
    } else {
      assert.equal(await complete('multipart-resume-full'), version);
    }
    assert.deepEqual(requests, [
      `PUT partNumber=1&${uploadQuery}`, `PUT partNumber=2&${uploadQuery}`, `POST ${uploadQuery}`,
    ]);
  });
}

const readbackPageVersion = 'page+%2F/=';
const readbackPageQuery = 'versionId=page%2B%252F%2F%3D';
const readbackHeaders = (bytes, id) => ({
  'content-length': String(bytes.length), 'x-amz-version-id': id,
});

function mockReadback(t, { mediaReply, pageReply, waitForPage = false } = {}) {
  let media;
  let page;
  let releaseCiphertext;
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
          controller.enqueue(media.subarray(7, wireHeaderLength));
          const finish = () => {
            controller.enqueue(media.subarray(wireHeaderLength));
            controller.close();
          };
          if (waitForPage) releaseCiphertext = finish;
          else finish();
        },
      });
      return new Response(body, { headers });
    }
    await verifySignedRequest(request, 'GET', readbackPageQuery, {}, sidecarKey);
    gets.push(sidecarKey);
    releaseCiphertext?.();
    const headers = readbackHeaders(page, readbackPageVersion);
    return pageReply ? pageReply(page, headers) : new Response(page, { headers });
  });
  return gets;
}

test('streams the completed media and sidecar versions into authenticated readback', async (t) => {
  // Only the wire header is available until the verifier opens the sidecar.
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

test('does not renew the deadline between GETs or chunk reads', async (t) => {
  // Each operation advances the clock by less than the 2 s fixture budget.
  // Date is mocked so the cumulative-deadline assertion is independent of host speed.
  await t.test('across GETs', async (t) => {
    t.mock.timers.enable({ apis: ['Date'], now: Date.now() });
    const delayed = (bytes, headers) => {
      t.mock.timers.tick(1100);
      return new Response(bytes, { headers });
    };
    const gets = mockReadback(t, { mediaReply: delayed, pageReply: delayed });
    await assert.rejects(complete('readback'), rejectedWith('Body(Transport)'));
    assert.deepEqual(gets, [mediaKey, sidecarKey]);
  });
  await t.test('across chunk reads', async (t) => {
    t.mock.timers.enable({ apis: ['Date'], now: Date.now() });
    let pulls = 0;
    const gets = mockReadback(t, {
      mediaReply(bytes, headers) {
        const body = new ReadableStream({
          pull(controller) {
            t.mock.timers.tick(1100);
            if (++pulls === 1) {
              controller.enqueue(bytes.subarray(0, 7));
            } else {
              controller.enqueue(bytes.subarray(7));
              controller.close();
            }
          },
        }, { highWaterMark: 0 });
        return new Response(body, { headers });
      },
    });
    await assert.rejects(complete('readback'), rejectedWith('Body(Transport)'));
    assert.equal(pulls, 2);
    assert.deepEqual(gets, [mediaKey]);
  });
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

test('refuses an unrelated checkpoint, durable target, or provider before uploading the part', async (t) => {
  const fetch = t.mock.method(globalThis, 'fetch', () => { throw new Error('unexpected fetch'); });
  for (const mode of ['part-other', 'part-context', 'part-durable-id', 'part-durable-session']) {
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

const listQuery = `max-uploads=2&prefix=${encodeURIComponent(mediaKey)}&uploads=`;
const namespace = 'http://s3.amazonaws.com/doc/2006-03-01/';
const listedUpload = (key, id) => `<Upload><Key>${key}</Key><UploadId>${id}</UploadId>`
  + '<Initiator><ID>synthetic</ID></Initiator><Owner><ID>synthetic</ID></Owner>'
  + '<StorageClass>STANDARD</StorageClass><Initiated>2026-11-24T00:00:00Z</Initiated></Upload>';
const listing = (uploads, overrides = {}) => {
  const { name = bucket, prefix = mediaKey, truncated = 'false', ns = namespace } = overrides;
  return `<ListMultipartUploadsResult xmlns="${ns}"><Bucket>${name}</Bucket><Prefix>${prefix}</Prefix>`
    + `<KeyMarker/><UploadIdMarker/><MaxUploads>2</MaxUploads><IsTruncated>${truncated}</IsTruncated>`
    + `${uploads}</ListMultipartUploadsResult>`;
};

test('reconciles a sole exact upload and safely retries an interrupted abort', async (t) => {
  const suffix = `${mediaKey}-other`;
  const open = new Map([[mediaKey, initiatedId], [suffix, 'unrelated']]);
  const versions = new Map([[mediaKey, version]]);
  const fetch = t.mock.method(globalThis, 'fetch', async (request) => {
    if (request.method === 'GET') {
      assert.equal((await verifySignedRequest(request, 'GET', listQuery, {}, '')).length, 0);
      const completePage = listing([...open].map(([key, id]) => listedUpload(key, id)).join(''))
        .replace('<MaxUploads>', `<NextKeyMarker>${suffix}</NextKeyMarker>`
          + '<NextUploadIdMarker>unrelated</NextUploadIdMarker><MaxUploads>');
      return new Response(completePage);
    }
    assert.equal((await verifySignedRequest(request, 'DELETE', initiatedQuery, {})).length, 0);
    if (open.delete(mediaKey)) {
      throw new Error('synthetic loss of abort response');
    }
    return new Response('<Error><Code>NoSuchUpload</Code><Message>Already absent</Message></Error>',
      { status: 404 });
  });
  await assert.rejects(complete('reconcile'), rejectedWith('unconfirmed abort'));
  assert.equal(await complete('reconcile-replay'), initiatedId);
  assert.equal(fetch.mock.callCount(), 3);
  assert.deepEqual([...open], [[suffix, 'unrelated']]);
  assert.equal(versions.get(mediaKey), version);
});

test('accepts the empty 204 response to an exact signed abort', async (t) => {
  const fetch = t.mock.method(globalThis, 'fetch', async (request) => {
    await verifySignedRequest(request, 'DELETE', initiatedQuery, {});
    return new Response(null, { status: 204 });
  });
  assert.equal(await complete('reconcile-replay'), initiatedId);
  assert.equal(fetch.mock.callCount(), 1);
});

test('ambiguous or wrong-scope listings cannot reach abort', async (t) => {
  const exact = listedUpload(mediaKey, initiatedId);
  let reply;
  const fetch = t.mock.method(globalThis, 'fetch', async (request) => {
    await verifySignedRequest(request, 'GET', listQuery, {}, '');
    return new Response(reply);
  });
  for (const body of [
    listing(''),
    listing(listedUpload(`${mediaKey}-other`, 'unrelated')),
    listing(exact + exact),
    listing(exact + listedUpload('media/other', 'unrelated')),
    listing(exact, { truncated: 'true' }),
    listing(exact, { name: 'other-bucket' }),
    listing(exact, { prefix: 'media/other' }),
    listing(exact, { ns: 'https://other.test/' }),
    listing(exact + '<Error><Code>InternalError</Code></Error>'),
    listing(exact.replace('</Upload>', '<Error><Code>InternalError</Code></Error></Upload>')),
    listing(exact.replace('<UploadId>', '<UploadId>split</UploadId><UploadId>')),
    listing(exact.replace('<UploadId>', '<UploadId><Nested/>')),
    listing(exact.replace(initiatedId, 'x'.repeat(16 * 1024))),
    listing(exact) + '<Error><Code>NoSuchUpload</Code></Error>',
  ]) {
    reply = body;
    await assert.rejects(complete('reconcile'), rejectedWith('unconfirmed listing'));
  }
  assert.equal(fetch.mock.callCount(), 14);
  await assert.rejects(complete('reconcile-expired'), rejectedWith('unconfirmed listing'));
  assert.equal(fetch.mock.callCount(), 14);
});

test('an ambiguous or mismatched abort response stays unconfirmed', async (t) => {
  let reply;
  t.mock.method(globalThis, 'fetch', async (request) => {
    await verifySignedRequest(request, 'DELETE', initiatedQuery, {});
    return new Response(reply, { status: 404 });
  });
  for (const body of [
    '',
    '<Error><Code>NoSuchBucket</Code></Error>',
    '<Error><Code>NoSuchUpload</Code><Key>media/other</Key></Error>',
    '<Error><Code>NoSuchUpload</Code><UploadId>other</UploadId></Error>',
    '<Error><Code>NoSuchUpload</Code><Resource>/other-bucket/other-key</Resource></Error>',
    '<Error><Code>NoSuchUpload</Code><Code>NoSuchBucket</Code></Error>',
    '<Error><Code>NoSuchUpload</Code><Error><Code>InternalError</Code></Error></Error>',
  ]) {
    reply = body;
    await assert.rejects(complete('reconcile-replay'), rejectedWith('unconfirmed abort'));
  }
});
