'use strict';

const assert = require('node:assert/strict');
const { test } = require('node:test');
const { s3_completion_test: complete } = require('./ingest_wasm.cjs');
const { bucket, sidecarKey } = require('./s3_fixture_protocol.cjs');

const version = 'Opaque+%2F/=';

for (const length of ['0', '1', '0, 0', '18446744073709551616']) {
  test(`checks declared sidecar PUT response length ${length}`, async (t) => {
    const fetch = t.mock.method(globalThis, 'fetch', async (request) => {
      assert.equal(request.method, 'PUT');
      assert.equal(request.url, `https://fixture.test:8443/${bucket}/${sidecarKey}`);
      return new Response(null, {
        headers: { 'content-length': length, 'x-amz-version-id': version },
      });
    });
    if (length === '0') {
      assert.equal(JSON.parse(await complete('sidecar')).version, version);
    } else {
      await assert.rejects(complete('sidecar'), (error) => error === 'Receipt(Response)');
    }
    assert.equal(fetch.mock.callCount(), 1);
  });
}
