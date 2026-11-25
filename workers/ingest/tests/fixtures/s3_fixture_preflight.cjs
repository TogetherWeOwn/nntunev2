'use strict';

const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const { test } = require('node:test');
const { qualify } = require('../../../../.github/scripts/private_trial_provider_preflight.cjs');
const { authorization, mediaKey, sha256 } = require('./s3_fixture_protocol.cjs');

const endpoint = 'https://fixture.test:8443';
const region = 'fixture-region';
const bucket = 'nntunev2-private-fixtures-staging';
const generation = 'cdcdcdcdcdcd40cd80cdcdcdcdcdcdcd';
const version = 'disposable+/=?';
const key = `media/00000000000040008000000000000000/${generation}`;
const path = `/${bucket}/${key}`;
const query = `versionId=${encodeURIComponent(version)}`;
const url = `${endpoint}${path}?${query}`;
const credential = { access_key: 'FIXTUREACCESS', secret_key: 'fixture-secret' };
const methods = ['HEAD', 'DELETE', 'HEAD', 'DELETE', 'HEAD'];

async function exercise(t, scenario = {}) {
  const scope = { bucket, endpoint, region, generation, version, ...scenario.scope };
  const inventory = new Map([[key, new Set([version, 'other-version'])], [mediaKey, new Set([version])]]);
  const requests = [];
  let deleted = false;
  t.mock.method(globalThis, 'fetch', async (request) => {
    const step = requests.push(request) - 1;
    if (request.url !== url || request.method !== methods[step]) throw new Error('outside synthetic selection');
    const selected = inventory.get(key);
    const present = selected.has(version);
    const reply = {
      status: request.method === 'DELETE' ? 204 : present ? 200 : 404,
      headers: step % 2 === 1 || present ? { 'x-amz-version-id': version } : {},
      url,
      ...scenario.at?.[step],
    };
    if (request.method === 'DELETE' && reply.status === 204) {
      selected.delete(version);
      deleted = true;
    }
    // Fetch disallows constructing a 204 with a body. Inject that malformed reply for refusal coverage.
    const response = new Response(reply.body, {
      status: reply.body && reply.status === 204 ? 200 : reply.status,
      headers: reply.headers,
    });
    Object.defineProperties(response, { url: { value: reply.url }, status: { value: reply.status } });
    return response;
  });
  const result = await qualify(process.env.INGEST_WASM_JS, endpoint, region,
    JSON.stringify(scope), JSON.stringify(credential));
  assert.equal(result, scenario.result ?? 'qualified');
  assert.deepEqual(requests.map(request => request.method), methods.slice(0, scenario.steps ?? methods.length));
  for (const request of requests) {
    assert.equal(request.url, url);
    assert.equal(request.redirect, 'manual');
    assert.equal(request.cache, 'no-store');
    assert.equal((await request.arrayBuffer()).byteLength, 0);
    const fields = {
      host: 'fixture.test:8443',
      'x-amz-content-sha256': sha256(Buffer.alloc(0)),
      'x-amz-date': request.headers.get('x-amz-date'),
    };
    assert.equal(request.headers.get('authorization'),
      authorization(request.method, path, query, fields, credential.access_key, credential.secret_key, region));
  }
  assert.deepEqual([...inventory.get(key)], deleted ? ['other-version'] : [version, 'other-version']);
  assert.deepEqual([...inventory.get(mediaKey)], [version]);
}

test('synthetic provider proof reads, deletes and reconfirms only the selected disposable version', async (t) => {
  await exercise(t, { at: { 2: { headers: { 'x-amz-version-id': version } }, 3: { headers: {} } } });
});

test('synthetic provider proof stops on invalid selection or ambiguous replies', async (t) => {
  const wrongVersion = { 'x-amz-version-id': 'other-version' };
  const cases = [
    ['scope origin differs', { scope: { endpoint: 'https://foreign.test' }, steps: 0, result: 'refused' }],
    ['scope region differs', { scope: { region: 'foreign-region' }, steps: 0, result: 'refused' }],
    ['scope bucket differs', { scope: { bucket: 'foreign-bucket' }, steps: 0, result: 'refused' }],
    ['key injection', { scope: { generation: '../' + mediaKey }, steps: 0, result: 'refused' }],
    ['unversioned selection', { scope: { version: 'null' }, steps: 0, result: 'refused' }],
    ['HEAD denied', 0, { status: 403 }],
    ['selected version absent at entry', 0, { status: 404 }],
    ['missing present version', 0, { headers: {} }],
    ['wrong present version', 0, { headers: wrongVersion }],
    ['duplicate present version', 0, { headers: [['x-amz-version-id', version], ['x-amz-version-id', version]] }],
    ['wrong response origin', 0, { url: url.replace(endpoint, 'https://foreign.test') }],
    ['redirect', 0, { status: 307 }],
    ['DELETE denied', 1, { status: 403 }],
    ['wrong DELETE version', 1, { headers: wrongVersion }],
    ['nonempty DELETE', 1, { body: 'unexpected' }],
    ['ambiguous 204 framing', 1, { headers: { 'content-length': '1' } }],
    ['post-delete HEAD denied', 2, { status: 403 }],
    ['nonempty HEAD 404', 2, { body: 'unexpected' }],
    ['wrong absent version', 2, { headers: wrongVersion }],
    ['repeat DELETE denied', 3, { status: 403 }],
    ['repeat HEAD reports present', 4, { status: 200 }],
  ];
  for (const [name, step, reply] of cases) {
    await t.test(name, (t) => exercise(t, reply
      ? { at: { [step]: reply }, steps: step + 1, result: 'unconfirmed' }
      : step));
  }
});

test('the operator entry requires explicit provider invocation and prints a fixed verdict', () => {
  const launcher = require.resolve('../../../../.github/scripts/private_trial_provider_preflight.cjs');
  const child = spawnSync(process.execPath, [launcher, process.env.INGEST_WASM_JS], { encoding: 'utf8', env: {} });
  assert.equal(child.status, 1);
  assert.equal(child.stdout, 'Private staging fixture provider preflight: refused.\n');
  assert.equal(child.stderr, '');
});
