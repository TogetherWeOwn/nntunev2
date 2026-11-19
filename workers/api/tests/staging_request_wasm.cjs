'use strict';

const assert = require('node:assert/strict');
const { test } = require('node:test');
const { staging_input_test: collect } = require(process.env.STAGING_BODY_WASM_JS);

const url = 'https://example.test/rest/ping';
const formRequest = ({ query = 'f=json', form = 'u=%C3%A9', declared } = {}) => new Request(`${url}?${query}`, {
  method: 'POST', body: form,
  headers: {
    'content-type': 'application/x-www-form-urlencoded',
    ...(declared === undefined ? {} : { 'content-length': declared }),
  },
});
const tooLarge = (e) => e === 'collect:Rejected(RequestOrDocumentTooLarge)';
const noCapacity = (e) => e === 'collect:Rejected(BusyOrCapacity)';

// Pinned wasm32 charge: two minimum chunks for path and query, one 8 KiB
// form reservation, then three minimum chunks for f, u and u's encoded value capacity.
const raw = 24 + 24 + 8208;
const exact = raw + 3 * 24;

test('carries one budget from collection through the permit into credential decoding', async () => {
  assert.equal(await collect(formRequest()), exact);
  const short = formRequest();
  await assert.rejects(collect(short, exact - 1), (e) => e === 'credentials:Capacity');
  assert.equal(short.bodyUsed, true);
  // A decode failure releases the construction slot; the next request can enter.
  assert.equal(await collect(formRequest(), exact), exact);
  await assert.rejects(collect(formRequest(), raw - 1), noCapacity);
});

test('admits path-only and query-only GET without a form reservation', async () => {
  assert.equal(await collect(new Request(url), 24), 24);
  assert.equal(await collect(new Request(`${url}?u=%C3%A9`), 96), 96);
});

test('rejects metadata errors before reading the form', async () => {
  const oversize = formRequest({ declared: '8193' });
  await assert.rejects(collect(oversize, 0), tooLarge);
  assert.equal(oversize.bodyUsed, false);
  const overParams = formRequest({ query: 'x=1&'.repeat(65) });
  await assert.rejects(collect(overParams), tooLarge);
  assert.equal(overParams.bodyUsed, false);
  const short = formRequest();
  await assert.rejects(collect(short, 24 + 24 - 1), noCapacity);
  assert.equal(short.bodyUsed, false);
});

test('applies parameter limits independently to query and form', async () => {
  const sixtyFour = 'x=1&'.repeat(64);
  // Every key is decoded and charged, even though x is not a credential.
  assert.equal(await collect(formRequest({ query: sixtyFour, form: sixtyFour })), 24 + 272 + 8208 + 128 * 24);
  await assert.rejects(collect(formRequest({ query: sixtyFour, form: `${sixtyFour}x=1` })), tooLarge);
});
