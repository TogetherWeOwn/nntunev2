'use strict';

const { registerHooks } = require('node:module');

// Load the Worker with runtime modules that refuse calls outside the fixture.
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
try {
  module.exports = require(process.env.INGEST_WASM_JS);
} finally {
  hooks.deregister();
}
