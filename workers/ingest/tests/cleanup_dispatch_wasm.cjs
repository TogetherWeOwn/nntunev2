'use strict';

const assert = require('node:assert/strict');
const { registerHooks } = require('node:module');
const { test } = require('node:test');

// The linked Worker imports runtime modules outside the queue adapter.
const workerStubs = new Map([
  ['cloudflare:sockets', 'export const connect = refuse;'],
  ['cloudflare:workers', 'export const tracing = new Proxy({}, { get: refuse });'],
]);
const hooks = registerHooks({
  resolve(specifier, context, nextResolve) {
    if (workerStubs.has(specifier)) {
      return { url: specifier, format: 'module', shortCircuit: true };
    }
    return nextResolve(specifier, context);
  },
  load(url, context, nextLoad) {
    const stub = workerStubs.get(url);
    if (stub !== undefined) {
      return {
        format: 'module',
        source: `const refuse = () => { throw new Error('unexpected ${url} access'); };\n${stub}`,
        shortCircuit: true,
      };
    }
    return nextLoad(url, context);
  },
});
let enqueue;
try {
  ({ cleanup_dispatch_test: enqueue } = require(process.env.INGEST_CLEANUP_WASM_JS));
} finally {
  hooks.deregister();
}

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
