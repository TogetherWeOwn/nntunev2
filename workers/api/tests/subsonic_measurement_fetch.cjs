'use strict';

const assert = require('node:assert/strict');
const { once } = require('node:events');
const { readFileSync, writeFileSync } = require('node:fs');
const { createRequire } = require('node:module');
const { createServer } = require('node:net');
const { resolve } = require('node:path');

async function main() {
  const [, , wrangler, origin] = process.argv;
  const { unstable_startWorker: startWorker } = createRequire(wrangler)('wrangler');
  const [matching, other] = JSON.parse(readFileSync(0, 'utf8'));
  const headers = { authorization: `Bearer ${process.env.QA_AUTH_TOKEN}`, 'x-subsonic-measurement-case': 'small' };
  const release = '00000000-0000-4000-8000-000000000002';
  const fixedCapacity = '{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":0,"message":"Service unavailable."}}}';
  const start = (variant) => startWorker({
    config: resolve(`measurement-${variant}.toml`),
    dev: { remote: false, server: { hostname: '127.0.0.1', port: 0 },
      origin: { secure: true, hostname: new URL(origin).hostname } },
  });
  const fetchWith = (worker) => (endpoint, params, headers, method = 'POST') => {
    const url = new URL(`/rest/${endpoint}`, origin);
    const form = new URLSearchParams(params);
    if (method === 'GET') url.search = form; // Disposable CI credentials only.
    return worker.fetch(url.href, {
      method, body: method === 'POST' ? form : undefined, headers,
      redirect: 'error', signal: AbortSignal.timeout(15_000),
    });
  };

  for (const enabled of [1, 0]) {
    const worker = await start(enabled);
    try {
      const fetchCase = fetchWith(worker);
      for (const [key, code] of enabled ? [[matching, null], [other, 50]] : [[matching, 44]]) {
        const response = await fetchCase('getAlbum.view', { f: 'json', apiKey: key, id: release }, headers);
        assert.equal(response.status, 200, `measurement ${enabled}, expected protocol code ${code}`);
        const result = (await response.json())['subsonic-response'];
        assert.equal(result.status, code === null ? 'ok' : 'failed');
        assert.equal(result.error?.code ?? null, code);
        assert.equal(result.album?.song?.[0]?.title ?? null, code === null ? 'Fixture "Track"' : null);
      }
      if (!enabled) continue;
      const select = (id) => ({ ...headers, 'x-subsonic-measurement-case': id });
      for (const requestHeaders of [{ authorization: headers.authorization }, select('rows-exact'),
        select('small, small'), { 'x-subsonic-measurement-case': 'small' }]) {
        const rejected = await fetchCase('getArtists', { f: 'json', apiKey: matching }, requestHeaders);
        assert.equal(rejected.status, 404);
        assert.equal(rejected.headers.get('cache-control'), 'private, no-store');
        assert.match(await rejected.text(), /Endpoint not supported in staging\./);
      }
      const unauthenticated = await fetchCase('getArtists', { f: 'json' }, headers);
      assert.equal(unauthenticated.status, 200);
      assert.equal((await unauthenticated.json())['subsonic-response'].error.code, 40);
      const empty = await fetchCase('getArtists', { f: 'json', apiKey: matching }, select('empty'));
      assert.equal(empty.status, 200);
      assert.deepEqual((await empty.json())['subsonic-response'].artists.index, []);
      for (const [endpoint, params] of [
        ['getAlbum.view', { f: 'json', id: release }],
        ['getArtists', { f: 'jsonp', callback: 'fixture.cb' }],
      ]) {
        const rejected = await fetchCase(endpoint, { ...params, apiKey: matching }, select('charge-exact'));
        assert.equal(rejected.status, 503, `${endpoint} ${params.f}`);
        assert.equal(rejected.headers.get('retry-after'), null);
        assert.equal(await rejected.text(), fixedCapacity);
      }
      let complete;
      const cases = [['small', 200], ['charge-below', 200], ['charge-exact', 200], ['charge-over', 503]];
      for (const [method, id, status] of ['POST', 'GET'].flatMap((method) => cases.map((c) => [method, ...c]))) {
        const response = await fetchCase('getArtists', { f: 'json', apiKey: matching }, select(id), method);
        assert.equal(response.status, status, id);
        assert.equal(response.headers.get('retry-after'), status === 503 ? '1' : null, id);
        assert.equal(response.headers.get('cache-control'), 'private, no-store', id);
        const body = await response.text();
        assert.equal(Buffer.byteLength(body), status === 503 ? 112 : 296, id);
        if (status === 503) {
          assert.equal(body, fixedCapacity, id);
        } else {
          const payload = JSON.parse(body)['subsonic-response'];
          assert.equal(payload.status, 'ok', id);
          assert.deepEqual(payload.artists.index[0].artist, [{
            id: '00000000-0000-4000-8000-000000000001', name: 'Fixture 雪 & Wind', albumCount: 1,
          }], id);
          if (complete) assert.equal(body, complete, id);
          complete = body;
        }
      }
    } finally {
      await worker.dispose();
    }
  }

  // Accept startup on loopback but never send a PostgreSQL response.
  const sockets = new Set();
  let connections = 0;
  let startups = 0;
  const peer = createServer((socket) => {
    connections += 1;
    sockets.add(socket);
    socket.once('data', () => { startups += 1; });
    socket.on('error', () => socket.destroy());
    socket.once('close', () => sockets.delete(socket));
  });
  const listening = once(peer, 'listening');
  peer.listen(0, '127.0.0.1');
  await listening;
  try {
    const config = readFileSync('measurement-1.toml', 'utf8').replace(
      /^localConnectionString = .*$/m,
      // The local Hyperdrive binding requires a password field even though
      // the silent peer never requests authentication.
      `localConnectionString = "postgresql://fixture:unused@127.0.0.1:${peer.address().port}/fixture?sslmode=disable"`,
    );
    // Keep the same enabled deployment and binding. Only the absolute UTC
    // lease window changes; neither the runner nor cleanup supplies a clock.
    const expired = config.replace(
      /^(SUBSONIC_MEASUREMENT_LEASE = "v1:[a-f0-9]{40}:)\d+:\d+(:.*)$/m,
      (_, prefix, suffix) => `${prefix}1000:2200${suffix}`,
    );
    assert.notEqual(expired, config, 'test must replace the UTC lease window');
    writeFileSync('measurement-expired.toml', expired);
    const expiredWorker = await start('expired');
    try {
      const response = await fetchWith(expiredWorker)('getAlbum.view', { f: 'json', apiKey: matching, id: release }, headers);
      assert.equal(response.status, 200);
      assert.equal(response.headers.get('cache-control'), 'private, no-store');
      const result = (await response.json())['subsonic-response'];
      assert.equal(result.status, 'failed');
      assert.equal(result.error?.code, 44);
      assert.equal(result.album, undefined);
    } finally {
      await expiredWorker.dispose();
    }
    assert.equal(connections, 0, 'expired lease must not open the measurement binding');
    console.log('ok: armed release Fetch rejects past UTC lease without cleanup or a measurement socket');

    writeFileSync('measurement-stalled.toml', config);
    const worker = await start('stalled');
    try {
      const started = performance.now();
      const response = await fetchWith(worker)('getAlbum.view', { f: 'json', apiKey: matching, id: release }, headers);
      assert.equal(response.status, 503);
      assert.equal(response.headers.get('retry-after'), '1');
      assert.equal(response.headers.get('cache-control'), 'private, no-store');
      assert.equal(response.headers.get('content-type'), 'application/xml');
      assert.equal(await response.text(), '<subsonic-response status="failed" version="1.16.1"><error code="0" message="Service unavailable."/></subsonic-response>');
      const elapsed = performance.now() - started;
      // Allow the two-second close reserve and runner jitter, but not the
      // eight-second operation deadline.
      assert.ok(elapsed < 5_500, `stalled staging Fetch took ${Math.round(elapsed)} ms`);
      assert.equal(startups, 1, 'Fetch must reach PostgreSQL startup exactly once');
    } finally {
      await worker.dispose();
    }
  } finally {
    for (const socket of sockets) socket.destroy();
    await new Promise((done) => peer.close(done));
  }
}

void main();
