'use strict';

const assert = require('node:assert/strict');
const { test } = require('node:test');
const {
  staging_dispatch_test: dispatch,
  staging_measurement_admits: admits,
} = require(process.env.STAGING_BODY_WASM_JS);

const request = (path) => new Request(`https://example.test/rest/${path}`);
const neverOpen = () => assert.fail('app connector must not run');

test('only the exact armed staging deployment admits the QA runner', () => {
  const sha = 'abcd'.repeat(10);
  const env = {
    RP_ID: 'example.test', BUILD_SHA: sha, QA_CODE_SEAM: '1', QA_AUTH_TOKEN: 'fixture',
    SUBSONIC_MEASUREMENT_STAGE: 'staging', SUBSONIC_MEASUREMENT_ENABLE: '1',
    SUBSONIC_MEASUREMENT_LEASE: `v1:${sha}:1000:2200:fixture-user`,
    SUBSONIC_MEASUREMENT_FIXTURE: 'small',
  };
  const url = 'https://example.test/rest/ping';
  const req = (target = url, bearer = 'Bearer fixture') => new Request(target, {
    headers: { authorization: bearer, 'x-forwarded-host': 'example.test' },
  });
  assert.equal(admits(req(), env, 1000), true);
  assert.equal(admits(req(), env, 2199), true);
  assert.equal(admits(req(), env, 2200), false);
  assert.equal(admits(req(url, 'Bearer wrongxx'), env, 1000), false);
  assert.equal(admits(new Request(url), env, 1000), false);
  for (const target of ['http://example.test/rest/ping', 'https://other.example.test/rest/ping',
    'https://example.test:444/rest/ping', 'https://example.test/restless']) {
    assert.equal(admits(req(target), env, 1000), false);
  }
  assert.equal(admits(req(), {}, 1000), false);
  for (const overrides of [{ SUBSONIC_MEASUREMENT_STAGE: 'production' }, { QA_CODE_SEAM: '0' },
    { SUBSONIC_MEASUREMENT_ENABLE: '0' }, { SUBSONIC_MEASUREMENT_LEASE: undefined },
    { BUILD_SHA: 'f'.repeat(40) }, { SUBSONIC_MEASUREMENT_FIXTURE: 'unknown' }]) {
    assert.equal(admits(req(), { ...env, ...overrides }, 1000), false);
  }
});

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
    const denied = await dispatch(request(`${endpoint}?f=json`), () => true, false, true);
    assert.equal(JSON.parse(await refusal(denied, 200))['subsonic-response'].error.code, 50);
  }
  const allowed = await dispatch(request('getMusicFolders?f=json'), () => true, true);
  const payload = JSON.parse(await refusal(allowed, 200))['subsonic-response'];
  assert.equal(payload.status, 'ok');
  assert.deepEqual(payload.musicFolders.musicFolder, [{ id: 1, name: 'Music' }]);
});

test('authorized synthetic rows use the bounded snapshot and all three render formats', async () => {
  const route = 'getAlbum?id=00000000-0000-4000-8000-000000000002';
  const empty = await dispatch(request('getArtists?f=json'), () => true, true, false);
  const emptyPayload = JSON.parse(await refusal(empty, 200))['subsonic-response'];
  assert.equal(emptyPayload.status, 'ok');
  assert.equal(emptyPayload.artists.index?.length ?? 0, 0);

  const render = (format) => dispatch(request(`${route}${format}`), () => true, true, true);
  const json = await refusal(await render('&f=json'), 200);
  const { album, status } = JSON.parse(json)['subsonic-response'];
  assert.equal(status, 'ok');
  assert.equal(album.name, 'Fixture <Album>');
  assert.equal(album.artist, 'Fixture 雪 & Wind');
  assert.equal(album.song.length, 1);
  assert.equal(album.song[0].title, 'Fixture "Track"');
  assert.match(await refusal(await render(''), 200), /name="Fixture &lt;Album&gt;"/);
  const jsonp = await refusal(await render('&f=jsonp&callback=fixture.cb'), 200);
  assert.equal(jsonp, `/**/fixture.cb(${json});`);
});
