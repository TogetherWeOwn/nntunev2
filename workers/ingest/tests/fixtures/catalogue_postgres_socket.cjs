'use strict';

const assert = require('node:assert/strict');
const { createConnection } = require('node:net');
const { Duplex } = require('node:stream');

exports.connect = ({ hostname, port }, { secureTransport }) => {
  assert.equal(process.env.GITHUB_ACTIONS, 'true');
  assert.equal(process.env.RUNNER_ENVIRONMENT, 'github-hosted');
  assert.equal(process.env.DATABASE_URL, 'postgresql://postgres:ci-throwaway@localhost:5432/nntune_ci');
  assert.equal(hostname, '127.0.0.1');
  assert.equal(port, 5432);
  assert.equal(secureTransport, 'off');

  const socket = createConnection({ host: hostname, port });
  socket.setTimeout(10_000, () => socket.destroy(new Error('fixture PostgreSQL socket timed out')));
  const streams = Duplex.toWeb(socket);
  return { ...streams, close: async () => socket.destroy() };
};
