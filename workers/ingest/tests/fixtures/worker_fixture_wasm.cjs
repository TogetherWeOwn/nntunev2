'use strict';

const { registerHooks } = require('node:module');
const { pathToFileURL } = require('node:url');

module.exports = function loadWorker(path, postgres = false) {
  // Load the Worker with runtime modules that refuse calls outside the fixture.
  const workerStubs = new Map([
    ['cloudflare:workers', 'export const tracing = new Proxy({}, { get: refuse });'],
  ]);
  const hooks = registerHooks({
    resolve(specifier, context, nextResolve) {
      if (specifier === 'cloudflare:sockets') {
        const socketFixture = postgres
          ? './catalogue_postgres_socket.cjs'
          : '../../../stream/tests/fixtures/s3_fixture_sockets.cjs';
        return { url: pathToFileURL(require.resolve(socketFixture)).href, shortCircuit: true };
      }
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
    return require(path);
  } finally {
    hooks.deregister();
  }
};
