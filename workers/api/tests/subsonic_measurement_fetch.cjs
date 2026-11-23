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
  const [matching, other, runner, shared] = JSON.parse(readFileSync(0, 'utf8'));
  const headers = { authorization: `Bearer ${runner}`, 'x-subsonic-measurement-case': 'small' };
  const artist = '00000000-0000-4000-8000-000000000001';
  const release = '00000000-0000-4000-8000-000000000002';
  const fixedCapacity = '{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":0,"message":"Service unavailable."}}}';
  const start = (variant) => {
    console.log(`measurement Fetch: starting ${variant}`);
    return startWorker({
      config: resolve(`measurement-${variant}.toml`),
      dev: { remote: false, server: { hostname: '127.0.0.1', port: 0 },
        origin: { secure: true, hostname: new URL(origin).hostname } },
    });
  };
  const fetchWith = (worker) => (endpoint, params, headers, method = 'POST') => {
    const url = new URL(`/rest/${endpoint}`, origin);
    const form = new URLSearchParams(params);
    if (method === 'GET') url.search = form; // Disposable CI credentials only.
    return worker.fetch(url.href, {
      method, body: method === 'POST' ? form : undefined, headers,
      redirect: 'error', signal: AbortSignal.timeout(15_000),
    });
  };

  const albumParams = { f: 'json', apiKey: matching, id: release };
  const refusedRoutes = [['getAlbum', 'GET'], ['getAlbum.view', 'POST'],
    ['getAlbum/child', 'POST'], ['%67etAlbum', 'POST'], ['getalbum', 'POST']];
  const refusedRoute = async (worker, endpoint, method) => {
    const response = await fetchWith(worker)(endpoint, method === 'GET' ? { f: 'json' } : albumParams,
      headers, method);
    return [response.status, await response.text()];
  };
  const ordinary = [];

  for (const enabled of [1, 0]) {
    const worker = await start(enabled);
    try {
      const fetchCase = fetchWith(worker);
      for (const [key, code] of enabled ? [[matching, null], [other, 50]] : [[matching, 44]]) {
        const response = await fetchCase('getAlbum', { f: 'json', apiKey: key, id: release }, headers);
        assert.equal(response.status, 200, `measurement ${enabled}, expected protocol code ${code}`);
        const result = (await response.json())['subsonic-response'];
        assert.equal(result.status, code === null ? 'ok' : 'failed');
        assert.equal(result.error?.code ?? null, code);
        assert.equal(result.album?.song?.[0]?.title ?? null, code === null ? 'Fixture "Track"' : null);
      }
      if (!enabled) {
        // Capture the ordinary responses without running two Wrangler sessions at once.
        for (const [endpoint, method] of refusedRoutes) {
          ordinary.push(await refusedRoute(worker, endpoint, method));
        }
        continue;
      }
      const artistResponse = await fetchCase('getArtist', { f: 'json', apiKey: matching, id: artist }, headers);
      assert.equal(artistResponse.status, 200);
      assert.equal((await artistResponse.json())['subsonic-response'].artist.name, 'Fixture 雪 & Wind');
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
        ['getAlbum', { f: 'json', id: release }],
        ['getArtists', { f: 'jsonp', callback: 'fixture.cb' }],
      ]) {
        const rejected = await fetchCase(endpoint, { ...params, apiKey: matching }, select('charge-exact'));
        assert.equal(rejected.status, 503, `${endpoint} ${params.f}`);
        assert.equal(rejected.headers.get('retry-after'), null);
        assert.equal(await rejected.text(), fixedCapacity);
      }
      let complete;
      const cases = [['small', 200], ['charge-below', 200], ['charge-exact', 200], ['charge-over', 503]];
      for (const [id, status] of cases) {
        const response = await fetchCase('getArtists', { f: 'json', apiKey: matching }, select(id));
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
    // Shift the active window into the past without changing its duration.
    // The release Worker still uses its own UTC clock.
    const expired = config.replace(
      /^(SUBSONIC_MEASUREMENT_LEASE = "v1:[a-f0-9]{40}:)(\d+):(\d+)(:.*)$/m,
      (_, prefix, issued, expires, suffix) => `${prefix}1000:${1000 + (Number(expires) - Number(issued))}${suffix}`,
    );
    assert.notEqual(expired, config, 'test must replace the UTC lease window');
    writeFileSync('measurement-expired.toml', expired);
    const expiredWorker = await start('expired');
    try {
      const response = await fetchWith(expiredWorker)('getAlbum', { f: 'json', apiKey: matching, id: release }, headers);
      assert.equal(response.status, 200);
      assert.equal(response.headers.get('cache-control'), 'private, no-store');
      const envelope = await response.json();
      const { serverVersion } = envelope['subsonic-response'];
      assert.equal(typeof serverVersion, 'string');
      assert.deepEqual(envelope, {
        'subsonic-response': {
          status: 'failed', version: '1.16.1', type: 'nntune', serverVersion, openSubsonic: true,
          error: { code: 44, message: 'Invalid API key.' },
        },
      });
    } finally {
      await expiredWorker.dispose();
    }
    assert.equal(connections, 0, 'expired lease must not open the measurement binding');

    writeFileSync('measurement-stalled.toml', config);
    const devVars = readFileSync('.dev.vars', 'utf8');
    const expiredBearer = devVars.replace(/^SUBSONIC_MEASUREMENT_RUNNER=v1:\d+:\d+:/m,
      'SUBSONIC_MEASUREMENT_RUNNER=v1:1000:1600:');
    assert.notEqual(expiredBearer, devVars, 'test must expire the dedicated bearer');
    writeFileSync('.dev.vars', expiredBearer);
    try {
      const expiredRunnerWorker = await start('stalled');
      try {
        const response = await fetchWith(expiredRunnerWorker)('getArtists', { apiKey: matching }, headers);
        assert.equal(response.status, 404);
        assert.match(await response.text(), /Endpoint not supported in staging\./);
      } finally {
        await expiredRunnerWorker.dispose();
      }
    } finally {
      writeFileSync('.dev.vars', devVars);
    }
    const worker = await start('stalled');
    try {
      for (const [index, [endpoint, method]] of refusedRoutes.entries()) {
        const [status, body] = await refusedRoute(worker, endpoint, method);
        assert.equal(status, ordinary[index][0], `${method} ${endpoint}`);
        assert.ok(body === ordinary[index][1], `${method} ${endpoint} changed ordinary response`);
      }
      for (const authorization of [undefined, `Bearer ${shared}`, `Bearer ${'W'.repeat(43)}`]) {
        const attempt = { 'x-subsonic-measurement-case': 'small' };
        if (authorization) attempt.authorization = authorization;
        const response = await fetchWith(worker)('getAlbum', albumParams, attempt);
        assert.equal(response.status, 404);
        assert.match(await response.text(), /Endpoint not supported in staging\./);
      }
      assert.equal(connections, 0, 'all refusals must leave the fixture DB untouched');
      const started = performance.now();
      const response = await fetchWith(worker)('getAlbum', { f: 'json', apiKey: matching, id: release }, headers);
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
  // Catch late connection callbacks even if a socket sent no startup bytes.
  assert.equal(connections, 1, 'only the active stalled control may connect');
  console.log('ok: refused scope, expired lease and expired bearer never open a measurement socket');
}

void main();
