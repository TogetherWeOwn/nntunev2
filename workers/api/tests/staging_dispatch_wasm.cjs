'use strict';

const assert = require('node:assert/strict');
const { test } = require('node:test');
const { staging_dispatch_test: dispatch, staging_measurement_admits: admits, staging_fixture_charge: charge } = require(process.env.STAGING_BODY_WASM_JS);

const request = (path) => new Request(`https://example.test/rest/${path}`);
const neverOpen = () => assert.fail('app connector must not run');

test('only the exact armed staging deployment admits the QA runner', () => {
  const sha = 'abcd'.repeat(10);
  const lease = `v1:${sha}:1000:2200:fixture-user`;
  const env = {
    RP_ID: 'example.test', BUILD_SHA: sha, QA_CODE_SEAM: '1', QA_AUTH_TOKEN: 'fixture',
    SUBSONIC_MEASUREMENT_STAGE: 'staging', SUBSONIC_MEASUREMENT_ENABLE: '1',
    SUBSONIC_MEASUREMENT_LEASE: `${lease};empty,small,charge-below,charge-exact,charge-over`,
  };
  const url = 'https://example.test/rest/ping';
  const req = (target = url, bearer = 'Bearer fixture', id = 'small') => new Request(target, {
    headers: { authorization: bearer, 'x-subsonic-measurement-case': id, 'x-forwarded-host': 'example.test' },
  });
  for (const [now, expected] of [[1000, true], [2199, true], [2200, false]])
    assert.equal(admits(req(), env, now), expected);
  assert.equal(admits(req(url, 'Bearer wrongxx'), env, 1000), false);
  assert.equal(admits(new Request(url), env, 1000), false);
  for (const target of ['http://example.test/rest/ping', 'https://other.example.test/rest/ping',
    'https://example.test:444/rest/ping', 'https://example.test/restless']) {
    assert.equal(admits(req(target), env, 1000), false);
  }
  assert.equal(admits(req(), {}, 1000), false);
  for (const id of ['empty', 'small', 'charge-below', 'charge-exact', 'charge-over'])
    assert.equal(admits(req(url, undefined, id), env, 1000), true);
  const fullMatrix = ['empty', 'small', 'large-mixed',
    ...['rows', 'charge', 'text', 'output-json', 'output-xml', 'output-jsonp']
      .flatMap((prefix) => ['below', 'exact', 'over'].map((position) => `${prefix}-${position}`))];
  for (const id of fullMatrix) {
    assert.equal(admits(req(url, undefined, id), {
      ...env, SUBSONIC_MEASUREMENT_LEASE: `${lease};${fullMatrix.join(',')}`,
    }, 1000), true, id);
  }
  assert.equal(admits(req(`${url}?fixture=unknown&cap=0`), env, 1000), true);
  const missing = req(`${url}?case=small`);
  missing.headers.delete('x-subsonic-measurement-case');
  assert.equal(admits(missing, env, 1000), false);
  for (const id of ['', 'large-mixed', 'unknown', 'SMALL', 'small, small', 'x'.repeat(8192)])
    assert.equal(admits(req(url, undefined, id), env, 1000), false);
  for (const value of [undefined, lease, `${lease};`, `${lease};small,`, `${lease};small,small`,
    `${lease};small,unknown`, `${lease};field-exact`, `${lease};small;large-mixed`, 'a'.repeat(641)]) {
    assert.equal(admits(req(), { ...env, SUBSONIC_MEASUREMENT_LEASE: value }, 1000), false);
  }
  for (const overrides of [{ SUBSONIC_MEASUREMENT_STAGE: 'production' },
    { SUBSONIC_MEASUREMENT_STAGE: undefined }, { QA_CODE_SEAM: '0' }, { QA_AUTH_TOKEN: undefined },
    { SUBSONIC_MEASUREMENT_ENABLE: '0' }, { BUILD_SHA: 'f'.repeat(40) }]) {
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
    const denied = await dispatch(request(`${endpoint}?f=json`), () => true, false, 'small');
    assert.equal(JSON.parse(await refusal(denied, 200))['subsonic-response'].error.code, 50);
  }
  const allowed = await dispatch(request('getMusicFolders?f=json'), () => true, true);
  const payload = JSON.parse(await refusal(allowed, 200))['subsonic-response'];
  assert.equal(payload.status, 'ok');
  assert.deepEqual(payload.musicFolders.musicFolder, [{ id: 1, name: 'Music' }]);
});

test('authorized synthetic rows use the bounded snapshot and all three render formats', async () => {
  const route = 'getAlbum?id=00000000-0000-4000-8000-000000000002';
  const empty = await dispatch(request('getArtists?f=json'), () => true, true, 'empty');
  const emptyPayload = JSON.parse(await refusal(empty, 200))['subsonic-response'];
  assert.equal(emptyPayload.status, 'ok');
  assert.equal(emptyPayload.artists.index?.length ?? 0, 0);

  const render = (format) => dispatch(request(`${route}${format}`), () => true, true, 'small');
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

const album = 'getAlbum?id=00000000-0000-4000-8000-000000000002';
const artist = 'getArtist?id=00000000-0000-4000-8000-000000000001';
const fixtureResponse = (id, route) => dispatch(request(route), () => true, true, id);
const suffixes = ['below', 'exact', 'over'];

test('row, field and aggregate text presets reach the application boundary', async () => {
  for (const [kind, route, lengths] of [
    ['rows', `${artist}&f=json`, [492, 492]],
    ['field', `${album}&f=json`, [1991, 1992]],
    ['text', 'getArtists?f=json', [296, 296]],
  ]) {
    for (const [i, suffix] of suffixes.entries()) {
      const id = `${kind}-${suffix}`;
      const response = await fixtureResponse(id, route);
      if (i === 2) {
        assert.equal(Buffer.byteLength(await refusal(response, 503, '1')), 112, id);
        continue;
      }
      assert.equal(response.status, 200, id);
      const body = await response.text();
      assert.equal(Buffer.byteLength(body), lengths[i], id);
      const payload = JSON.parse(body)['subsonic-response'];
      assert.equal(payload.status, 'ok', id);
      if (kind === 'rows') assert.equal(payload.artist.album[0].songCount, 1023 + i, id);
      if (kind === 'field') assert.equal(Buffer.byteLength(payload.album.song[0].title), 1023 + i, id);
    }
  }
});

test('large mixed catalogue and exact document boundaries render through the bounded sink', async () => {
  for (const [format, selector, large, rejection] of [
    ['json', '&f=json', 106659, 122],
    ['xml', '', 118091, 130],
    ['jsonp', '&f=jsonp&callback=fixture.cb', 106676, 122],
  ]) {
    for (const [i, id] of ['large-mixed', ...suffixes.map((s) => `output-${format}-${s}`)].entries()) {
      const response = await fixtureResponse(id, album + selector);
      if (i === 3) {
        const body = await refusal(response, 413);
        assert.ok(!body.includes('fixture.cb'));
        assert.equal(Buffer.byteLength(body), rejection, id);
      } else {
        assert.equal(response.status, 200, id);
        const body = await response.text();
        assert.equal(Buffer.byteLength(body), [large, 262143, 262144][i], id);
        assert.ok(body.includes(format === 'xml' ? '雪&lt;&amp;&gt;&quot;' : '雪<&>'), id);
        if (format === 'jsonp') assert.ok(body.startsWith('/**/fixture.cb(') && body.endsWith(');'), id);
      }
    }
  }
});

test('large mixed raw-row charge admits equality on WASM and rejects one byte over', () => {
  const exact = charge(8 * 1024 * 1024);
  assert.equal(exact, 291928);
  assert.equal(charge(exact + 1), exact);
  assert.equal(charge(exact), exact);
  assert.equal(charge(exact - 1), undefined);
});
