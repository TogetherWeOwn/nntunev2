'use strict';

const assert = require('node:assert/strict');
const { test } = require('node:test');
const { cleanup_dispatch_test: enqueue } = require('./fixtures/ingest_wasm.cjs');

const sessionId = '00000000-0000-0000-0000-000000000001';
const claimId = '00000000-0000-0000-0000-000000000002';

for (const [kind, terminal, delaySeconds] of [
  ['absolute_expiry', false, 86400],
  ['terminal_reconcile', true, 0],
]) {
  test(`sends ${kind} through the ingest queue with its claim delay`, async () => {
    const sent = [];
    // The pinned Worker SDK checks the binding's constructor name.
    class WorkerQueue {
      async send(body, options) {
        sent.push({ body, options });
      }
    }

    await enqueue({ INGEST_QUEUE: new WorkerQueue() }, sessionId, claimId, terminal, delaySeconds);

    assert.deepEqual(sent, [{
      body: { type: 'upload_session_cleanup', session_id: sessionId, kind },
      options: { contentType: 'json', delaySeconds },
    }]);
  });
}
