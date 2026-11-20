'use strict';

const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { createRequire } = require('node:module');
const { resolve } = require('node:path');

async function main() {
  const [, , wrangler, origin] = process.argv;
  const { unstable_startWorker: startWorker } = createRequire(wrangler)('wrangler');
  const [matching, other] = JSON.parse(readFileSync(0, 'utf8'));
  const headers = { authorization: `Bearer ${process.env.QA_AUTH_TOKEN}` };
  for (const enabled of [1, 0]) {
    const worker = await startWorker({
      config: resolve(`measurement-${enabled}.toml`),
      dev: { remote: false, server: { hostname: '127.0.0.1', port: 0 },
        origin: { secure: true, hostname: new URL(origin).hostname } },
    });
    try {
      for (const [key, code] of enabled ? [[matching, null], [other, 50]] : [[matching, 44]]) {
        const url = new URL('/rest/getAlbum.view', origin);
        url.search = new URLSearchParams({ f: 'json', apiKey: key, id: '00000000-0000-4000-8000-000000000002' });
        const response = await worker.fetch(url.href, { headers, signal: AbortSignal.timeout(15_000) });
        assert.equal(response.status, 200, `measurement ${enabled}, expected protocol code ${code}`);
        const result = (await response.json())['subsonic-response'];
        assert.equal(result.status, code === null ? 'ok' : 'failed');
        assert.equal(result.error?.code ?? null, code);
        assert.equal(result.album?.song?.[0]?.title ?? null, code === null ? 'Fixture "Track"' : null);
      }
    } finally {
      await worker.dispose();
    }
  }
}

void main();
