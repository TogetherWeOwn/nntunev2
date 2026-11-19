'use strict';

const assert = require('node:assert/strict');
const { test } = require('node:test');
const { staging_rejection_test: reject } = require(process.env.STAGING_BODY_WASM_JS);

test('Worker rejections use bounded, non-executable responses', async () => {
  for (const busy of [false, true]) {
    for (const jsonp of [false, true]) {
      const response = reject(busy, jsonp);
      assert.equal(response.status, busy ? 503 : 413);
      assert.equal(response.headers.get('retry-after'), busy ? '1' : null);
      assert.equal(response.headers.get('cache-control'), 'private, no-store');
      const body = await response.text();
      assert.ok(Buffer.byteLength(body) <= 1024);
      if (jsonp) {
        assert.match(response.headers.get('content-type'), /^application\/json/);
        assert.equal(JSON.parse(body)['subsonic-response'].status, 'failed');
      } else {
        assert.match(body, /^<subsonic-response/);
      }
    }
  }
});
