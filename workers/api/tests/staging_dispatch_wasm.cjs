'use strict';

const assert = require('node:assert/strict');
const { test } = require('node:test');
const { staging_dispatch_test: dispatch } = require(process.env.STAGING_BODY_WASM_JS);

const request = (path) => new Request(`https://example.test/rest/${path}`);
const neverOpen = () => assert.fail('app connector must not run');

async function refusal(response, status, retry = null) {
  assert.equal(response.status, status);
  assert.equal(response.headers.get('retry-after'), retry);
  assert.equal(response.headers.get('cache-control'), 'private, no-store');
  const body = await response.text();
  assert.ok(Buffer.byteLength(body) <= 1024);
  return body;
}

test('rejects oversized input, unsupported routes and duplicate credentials before app access', async () => {
  await refusal(await dispatch(request(`ping?x=${'a'.repeat(8192)}`), neverOpen), 413);
  await refusal(await dispatch(request('stream?id=recording'), neverOpen), 404);
  const response = await dispatch(request('ping?f=jsonp&callback=fixture.cb&u=first&u=second'), neverOpen);
  assert.match(response.headers.get('content-type'), /^application\/json/);
  const body = await refusal(response, 200);
  assert.equal(JSON.parse(body)['subsonic-response'].error.code, 0);
  for (const input of ['fixture.cb', 'first', 'second']) assert.ok(!body.includes(input));
});

test('busy refusal and app-open timeout release the permit', { timeout: 10000 }, async () => {
  let entered;
  const started = new Promise((resolve) => { entered = resolve; });
  const first = dispatch(request('getArtists?apiKey=fixture'), () => {
    entered();
    return new Promise(() => {});
  });
  await started;
  await refusal(await dispatch(request('ping?apiKey=fixture'), neverOpen), 503, '1');
  await refusal(await first, 503, '1');
  // Only the connector's Unavailable response has no Retry-After header.
  await refusal(await dispatch(request('ping?apiKey=fixture'), () => {}), 503);
});

test('fixture authorization gates account and catalogue reads and permits charged dispatch', async () => {
  for (const endpoint of ['getLicense', 'getArtists']) {
    const denied = await dispatch(request(`${endpoint}?f=json`), () => true, false);
    assert.equal(JSON.parse(await refusal(denied, 200))['subsonic-response'].error.code, 50);
  }
  const allowed = await dispatch(request('getMusicFolders?f=json'), () => true, true);
  const payload = JSON.parse(await refusal(allowed, 200))['subsonic-response'];
  assert.equal(payload.status, 'ok');
  assert.deepEqual(payload.musicFolders.musicFolder, [{ id: 1, name: 'Music' }]);
});
