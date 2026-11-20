'use strict';

require('./staging_metadata_wasm.cjs');
require('./staging_response_wasm.cjs');

const assert = require('node:assert/strict');
const { test } = require('node:test');
const {
  collect_staging_test: collect,
  collect_staging_timeout_test: collectWithDeadline,
} = require(process.env.STAGING_BODY_WASM_JS);
const bytes = (s) => new TextEncoder().encode(s);
const request = (stream) => new Request('https://example.test/', {
  method: 'POST', body: stream, duplex: 'half',
});
const chunks = (...values) => new ReadableStream({
  start(controller) {
    for (const value of values) controller.enqueue(value);
    controller.close();
  },
});

test('collects bounded chunks through the Worker adapter', async () => {
  const text = 'x=' + 'a'.repeat(8190);
  for (const declared of [undefined, 0, 1, 8192]) {
    assert.equal(await collect(request(chunks(bytes('x='), bytes(text.slice(2)))), declared), text);
  }
});

test('only charges for nonempty input and rejects a budget one byte short', async () => {
  assert.equal(await collect(request(chunks(bytes(''))), 0, true), '');
  await assert.rejects(collect(request(chunks(bytes('x=a'))), 3, true), (e) => e === 'Capacity');
});

test('rejects oversized chunks and cumulative input', async () => {
  await assert.rejects(collect(request(chunks(bytes('x=' + 'a'.repeat(8191))))), (e) => e === 'TooLarge');
  await assert.rejects(collect(request(chunks(bytes('x='), bytes('a'.repeat(8190)), bytes('z')))), (e) => e === 'TooLarge');
  await assert.rejects(collect(request(chunks(bytes('x=1'))), 8193), (e) => e === 'TooLarge');
});

test('an oversized chunk cancels the open source and releases its reader lock', async () => {
  let cancelled = 0;
  const req = request(new ReadableStream({
    start(controller) { controller.enqueue(bytes('x=' + 'a'.repeat(8191))); },
    cancel() { cancelled++; throw new Error('test cancellation rejection'); },
  }));
  await assert.rejects(collect(req), (e) => e === 'TooLarge');
  assert.equal(cancelled, 1);
  assert.equal(req.body.locked, false);
});

test('request deadline cancels a blocked form reader and releases its lock', { timeout: 5000 }, async () => {
  let cancelled = 0;
  const req = request(new ReadableStream({
    cancel() { cancelled++; return new Promise(() => {}); },
  }));
  await assert.rejects(collectWithDeadline(req), (e) => e === 'Deadline');
  assert.equal(cancelled, 1);
  assert.equal(req.body.locked, false);
});

test('rejects short declarations, stream errors and non-byte chunks', async () => {
  await assert.rejects(collect(request(chunks(bytes('ab'))), 3), (e) => e === 'Unreadable');
  const failed = new ReadableStream({ start(controller) { controller.error(new Error('failed')); } });
  await assert.rejects(collect(request(failed)), (e) => e === 'Unreadable');
  await assert.rejects(collect(request(chunks('not bytes'))), (e) => e === 'Unreadable');
});
