'use strict';

const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { createRequire } = require('node:module');
const { resolve } = require('node:path');

async function main() {
  const [, , wrangler, origin] = process.argv;
  const { unstable_startWorker: startWorker } = createRequire(wrangler)('wrangler');
  const [matching, other] = JSON.parse(readFileSync(0, 'utf8'));
  const headers = { authorization: `Bearer ${process.env.QA_AUTH_TOKEN}`, 'x-subsonic-measurement-case': 'small' };
  const release = '00000000-0000-4000-8000-000000000002';
  const fixedCapacity = '{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":0,"message":"Service unavailable."}}}';
  for (const enabled of [1, 0]) {
    const worker = await startWorker({
      config: resolve(`measurement-${enabled}.toml`),
      dev: { remote: false, server: { hostname: '127.0.0.1', port: 0 },
        origin: { secure: true, hostname: new URL(origin).hostname } },
    });
    try {
      const fetchCase = (endpoint, params, requestHeaders) => {
        const url = new URL(`/rest/${endpoint}`, origin);
        return worker.fetch(url.href, {
          method: 'POST', body: new URLSearchParams(params), headers: requestHeaders,
          redirect: 'error', signal: AbortSignal.timeout(15_000),
        });
      };
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
      let complete;
      for (const [id, status] of [['small', 200], ['charge-below', 200], ['charge-exact', 200], ['charge-over', 503]]) {
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
}

void main();
