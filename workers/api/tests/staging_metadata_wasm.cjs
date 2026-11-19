'use strict';

const assert = require('node:assert/strict');
const { test } = require('node:test');
const { staging_metadata_test: admit } = require(process.env.STAGING_BODY_WASM_JS);
const origin = 'https://example.test';
const tooLarge = (e) => e === 'Rejected(RequestOrDocumentTooLarge)';
const capacity = (e) => e === 'Rejected(BusyOrCapacity)';

test('admits exact path, query and normalized header limits before the Rust copy', () => {
  const path = '/' + 'p'.repeat(1023);
  const query = 'x=' + 'q'.repeat(8190);
  const headers = { 'x-test': 'h'.repeat(8192 - 'x-test'.length - 4) };
  const request = new Request(`${origin}${path}?${query}`, { headers });
  // The pinned ledger rounds each aligned buffer and adds 16 bytes.
  const exact = (1024 + 16) + (8192 + 16);
  assert.deepEqual(admit(request, exact), ['GET', path, query, undefined, exact]);
  assert.throws(() => admit(request, exact - 1), capacity);
  assert.throws(() => admit(new Request(`${origin}${path}p`), 0), tooLarge);
  assert.throws(() => admit(new Request(`${origin}/?${query}q`), 0), tooLarge);
  headers['x-test'] += 'h';
  assert.throws(() => admit(new Request(`${origin}/`, { headers }), 0), tooLarge);
  assert.throws(() => admit(new Request(`${origin}/`, { method: 'HEAD' }), 0), (e) => e === 'UnsupportedMethod');
});

test('retains encoded URL bytes and admits form metadata without reading the stream', () => {
  const request = (query, declared) => new Request(`${origin}/é?${query}`, {
    method: 'POST', body: 'u=test',
    headers: {
      'content-type': 'application/x-www-form-urlencoded',
      ...(declared === undefined ? {} : { 'content-length': declared }),
    },
  });
  for (const [declared, expected] of [[undefined, null], ['8192', 8192]]) {
    const input = request('f=json', declared);
    const [method, path, query, length] = admit(input, 100);
    assert.deepEqual([method, path, query, length], ['POST', '/%C3%A9', 'f=json', expected]);
    assert.equal(input.bodyUsed, false);
  }
  for (const [query, declared, reject] of [
    ['f=json', '8193', tooLarge],
    ['f=json', '8192, 8192', (e) => e === 'Unreadable'],
    ['x=1&'.repeat(65), undefined, tooLarge],
  ]) {
    const input = request(query, declared);
    assert.throws(() => admit(input, 1024), reject);
    assert.equal(input.bodyUsed, false);
  }
});

test('skips form admission for a non-form POST and a GET with a form header', () => {
  const post = new Request(`${origin}/`, {
    method: 'POST', body: 'text',
    headers: { 'content-type': 'text/plain', 'content-length': '4' },
  });
  const get = new Request(`${origin}/`, {
    headers: { 'content-type': 'application/x-www-form-urlencoded', 'content-length': '0' },
  });
  assert.equal(admit(post, 100)[3], undefined);
  assert.equal(admit(get, 100)[3], undefined);
  assert.equal(post.bodyUsed, false);
});
