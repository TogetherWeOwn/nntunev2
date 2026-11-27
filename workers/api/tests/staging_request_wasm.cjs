'use strict';

const assert = require('node:assert/strict');
const { test } = require('node:test');
const { staging_input_test: collect } = require(process.env.STAGING_BODY_WASM_JS);

const url = 'https://example.test/rest/ping';
const formType = 'application/x-www-form-urlencoded';
const encodedForm = 'u=%C3%A9';
const formRequest = ({ query = 'f=json', form = encodedForm, declared, contentType = formType } = {}) => new Request(`${url}?${query}`, {
  method: 'POST', body: form,
  headers: {
    'content-type': contentType,
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
  assert.deepEqual(await collect(formRequest()), [exact, encodedForm]);
  const short = formRequest();
  await assert.rejects(collect(short, exact - 1), (e) => e === 'credentials:Capacity');
  assert.equal(short.bodyUsed, true);
  // A decode failure releases the construction slot; the next request can enter.
  assert.deepEqual(await collect(formRequest(), exact), [exact, encodedForm]);
  await assert.rejects(collect(formRequest(), raw - 1), noCapacity);
});

test('admits path-only and query-only GET without a form reservation', async () => {
  assert.deepEqual(await collect(new Request(url), 24), [24, '']);
  assert.deepEqual(await collect(new Request(`${url}?u=%C3%A9`), 96), [96, '']);
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

test('reads a form only for an unambiguous form media type', { timeout: 5000 }, async () => {
  const lookalike = formRequest({ contentType: `text/plain; hint=${formType}` });
  assert.deepEqual(await collect(lookalike), [24 + 24 + 24, '']);
  assert.equal(lookalike.bodyUsed, false);

  for (const contentType of [`${formType}, text/plain`, `${formType}; name=one",two"`]) {
    const malformed = formRequest({ contentType });
    await assert.rejects(collect(malformed), (e) => e === 'collect:Unreadable');
    assert.equal(malformed.bodyUsed, false);
  }

  const quoted = formRequest({ contentType: `${formType}; name="one,two"` });
  assert.deepEqual(await collect(quoted), [exact, encodedForm]);
  assert.equal(quoted.bodyUsed, true);
});

test('passes an admitted declared length to the form collector', async () => {
  const length = Buffer.byteLength(encodedForm);
  assert.deepEqual(await collect(formRequest({ declared: String(length) })), [exact, encodedForm]);
  const incomplete = formRequest({ declared: String(length + 1) });
  await assert.rejects(collect(incomplete), (e) => e === 'collect:Unreadable');
  assert.equal(incomplete.bodyUsed, true);
});

test('applies parameter limits independently to query and form', async () => {
  const sixtyFour = 'x=1&'.repeat(64);
  // Every key is decoded and charged, even though x is not a credential.
  assert.deepEqual(await collect(formRequest({ query: sixtyFour, form: sixtyFour })), [24 + 272 + 8208 + 128 * 24, sixtyFour]);
  await assert.rejects(collect(formRequest({ query: sixtyFour, form: `${sixtyFour}x=1` })), tooLarge);
});
