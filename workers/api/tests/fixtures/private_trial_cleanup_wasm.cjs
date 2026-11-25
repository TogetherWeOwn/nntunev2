'use strict';

const assert = require('node:assert/strict');
const { test } = require('node:test');
const loadWorker = require('../../../ingest/tests/fixtures/worker_fixture_wasm.cjs');
const { authorization, mediaKey, sha256, sidecarKey } = require('../../../ingest/tests/fixtures/s3_fixture_protocol.cjs');
const {
  private_trial_cleanup_test: cleanup,
  private_trial_cleanup_foreign_bucket_test: cleanupForeignBucket,
} = loadWorker(process.env.PRIVATE_TRIAL_DISPATCH_WASM_JS, true);

const bucket = 'nntunev2-private-fixtures-staging';
const mediaVersion = 'media-fixture+/=?';
const pageVersion = 'page-fixture+/=?';

test('reconfirms the same media and page versions after interrupted page confirmation', async (t) => {
  const mediaVersions = new Set([mediaVersion, 'media-other-version']);
  const pageVersions = new Set([pageVersion, 'page-other-version']);
  const planned = [
    ['DELETE', mediaKey, mediaVersion, mediaVersions], ['HEAD', mediaKey, mediaVersion, mediaVersions],
    ['DELETE', sidecarKey, pageVersion, pageVersions], ['HEAD', sidecarKey, pageVersion, pageVersions],
  ];
  let verified = 0;
  let interruptConfirmation = true;
  t.mock.method(globalThis, 'fetch', async (request) => {
    const [method, key, version, versions] = planned[verified % planned.length];
    const path = `/${bucket}/${key}`;
    const query = `versionId=${encodeURIComponent(version)}`;
    assert.equal(request.url, `https://fixture.test:8443${path}?${query}`);
    assert.equal(request.method, method);
    assert.equal(request.redirect, 'manual');
    assert.equal(request.cache, 'no-store');
    assert.equal((await request.arrayBuffer()).byteLength, 0);
    const fields = {
      host: 'fixture.test:8443',
      'x-amz-content-sha256': sha256(Buffer.alloc(0)),
      'x-amz-date': request.headers.get('x-amz-date'),
    };
    assert.equal(request.headers.get('authorization'),
      authorization(method, path, query, fields, 'FIXTUREACCESS', 'fixture-secret', 'fixture-region'));
    verified += 1;
    if (method === 'DELETE') {
      versions.delete(version);
      return new Response(null, { status: 204, headers: { 'x-amz-version-id': version } });
    }
    if (key === sidecarKey && interruptConfirmation) {
      return new Response(null, { status: 503 });
    }
    return new Response(null, { status: versions.has(version) ? 200 : 404 });
  });

  const retry = () => cleanup(bucket, mediaKey, mediaVersion, sidecarKey, pageVersion);
  await assert.rejects(retry(), (error) => error === 'Unconfirmed');
  assert.equal(verified, 4);
  interruptConfirmation = false;
  await retry();
  assert.equal(verified, 8);
  assert.deepEqual([...mediaVersions], ['media-other-version']);
  assert.deepEqual([...pageVersions], ['page-other-version']);
});

test('preserves transport refusal for a foreign bucket', async (t) => {
  const fetch = t.mock.method(globalThis, 'fetch', () => assert.fail('unexpected fetch'));
  await assert.rejects(cleanup('fixture-bucket', mediaKey, mediaVersion, sidecarKey, pageVersion),
    (error) => error === 'Refused');
  assert.equal(fetch.mock.callCount(), 0);
});

test('the runner refuses a foreign bucket before attempting SQL retirement', async (t) => {
  const fetch = t.mock.method(globalThis, 'fetch', () => assert.fail('unexpected fetch'));
  await assert.rejects(cleanupForeignBucket(), (error) => error === 'Refused');
  assert.equal(fetch.mock.callCount(), 0);
});
