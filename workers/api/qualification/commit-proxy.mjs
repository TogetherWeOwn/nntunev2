// Actions-only loopback PostgreSQL fault transport. Never logs wire payloads.
import assert from 'node:assert/strict';
import { createServer, createConnection } from 'node:net';
import { once } from 'node:events';
import { database } from './database.mjs';

const targets = {
  plan: /\bINSERT\s+INTO\s+ingest_preparation_plans\b/i,
  dispatch: /\bINSERT\s+INTO\s+ingest_file_dispatches\b/i,
  startup: /\bINSERT\s+INTO\s+posted_file_job_attempts\b/i,
  acknowledgement: /\bUPDATE\s+ingest_file_dispatches\s+SET\s+send_acknowledged\b/i,
};

export async function commitProxy() {
  const target = new URL(database); // database.mjs validates the disposable DSN.
  const sockets = new Set();
  let armed;
  let dropped = 0;
  let failed = false;
  const server = createServer(client => {
    const upstream = createConnection({ host: target.hostname, port: Number(target.port) });
    for (const socket of [client, upstream]) {
      sockets.add(socket);
      socket.on('error', () => {}); // Expected connection loss contains no diagnostic payload.
      socket.on('close', () => { sockets.delete(socket); client.destroy(); upstream.destroy(); });
    }
    let startup = true;
    let incoming = Buffer.alloc(0);
    let outgoing = Buffer.alloc(0);
    let selected;
    let committing = false;
    let cut = false;
    function refuse() {
      failed = true;
      cut = true;
      client.destroy();
      upstream.destroy();
    }
    client.on('data', chunk => {
      if (cut) return;
      incoming = Buffer.concat([incoming, chunk]);
      while (incoming.length >= (startup ? 4 : 5)) {
        const size = incoming.readUInt32BE(startup ? 0 : 1);
        if (size < (startup ? 8 : 4) || size > 16 * 1024 * 1024) return refuse();
        const length = size + (startup ? 0 : 1);
        if (incoming.length < length) return;
        const frame = incoming.subarray(0, length);
        incoming = incoming.subarray(length);
        if (startup) {
          // This harness requires plaintext loopback protocol, never TLS/auth fallback.
          if (frame.readUInt32BE(4) !== 196608) return refuse();
          startup = false;
        } else {
          const type = String.fromCharCode(frame[0]);
          let query;
          if (type === 'Q') query = frame.subarray(5, -1).toString('utf8');
          else if (type === 'P') {
            const start = frame.indexOf(0, 5) + 1;
            const end = frame.indexOf(0, start);
            if (start < 6 || end < start) return refuse();
            query = frame.subarray(start, end).toString('utf8');
          }
          if (query !== undefined) {
            if (armed && targets[armed].test(query)) selected = armed;
            if (/^\s*ROLLBACK\s*;?\s*$/i.test(query)) selected = undefined;
            if (/^\s*COMMIT\s*;?\s*$/i.test(query)) committing = true;
          }
        }
        upstream.write(frame);
      }
    });
    upstream.on('data', chunk => {
      if (cut) return;
      outgoing = Buffer.concat([outgoing, chunk]);
      while (outgoing.length >= 5) {
        const size = outgoing.readUInt32BE(1);
        if (size < 4 || size > 16 * 1024 * 1024) return refuse();
        const length = size + 1;
        if (outgoing.length < length) return;
        const frame = outgoing.subarray(0, length);
        outgoing = outgoing.subarray(length);
        // PostgreSQL has committed before emitting CommandComplete(COMMIT).
        // Drop that frame AND ReadyForQuery; the real Rust client cannot observe
        // success. This is not a trigger error or a discarded successful return.
        if (frame[0] === 67 && frame.subarray(5).equals(Buffer.from('COMMIT\0'))) {
          if (committing && selected && selected === armed) {
            dropped += 1;
            armed = undefined;
            cut = true;
            client.destroy();
            upstream.destroy();
            return;
          }
          selected = undefined;
          committing = false;
        }
        if (frame[0] === 69) { selected = undefined; committing = false; }
        client.write(frame);
      }
    });
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const url = new URL(database);
  url.hostname = '127.0.0.1';
  url.port = String(server.address().port);
  return {
    database: url.toString(),
    arm(name) {
      assert.ok(Object.hasOwn(targets, name));
      assert.equal(armed, undefined, 'previous commit fault must have fired');
      assert.equal(failed, false, 'wire parser must not fail closed before fault injection');
      armed = name;
      return dropped;
    },
    assertDropped(previous) {
      assert.equal(failed, false, 'well-formed PostgreSQL wire protocol required');
      assert.equal(armed, undefined, 'selected mutation must reach COMMIT completion');
      assert.equal(dropped, previous + 1, 'exactly one committed acknowledgement must be suppressed');
    },
    async close() {
      const closed = once(server, 'close');
      server.close();
      for (const socket of sockets) socket.destroy();
      await closed;
    },
  };
}
