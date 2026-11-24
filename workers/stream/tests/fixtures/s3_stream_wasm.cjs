'use strict';

const { registerHooks } = require('node:module');
const { pathToFileURL } = require('node:url');

// The production bundle also imports the existing NNTP socket adapter.
// Make that import fail on use while keeping real Fetch and loopback TLS.
const sockets = pathToFileURL(require.resolve('./s3_fixture_sockets.cjs')).href;
const hook = registerHooks({
  resolve(specifier, context, nextResolve) {
    if (specifier === 'cloudflare:sockets') return { url: sockets, shortCircuit: true };
    return nextResolve(specifier, context);
  },
});
try {
  module.exports = require(process.env.S3_FIXTURE_WASM);
} finally {
  hook.deregister();
}
