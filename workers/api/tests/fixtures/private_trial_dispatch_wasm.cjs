'use strict';

const assert = require('node:assert/strict');
const { test } = require('node:test');
const loadWorker = require('../../../ingest/tests/fixtures/worker_fixture_wasm.cjs');
const { private_trial_dispatch_test: dispatch } = loadWorker(process.env.PRIVATE_TRIAL_DISPATCH_WASM_JS);

const origin = 'https://staging.nntune.com';
const byteLimit = 177175;
const request = (length) => new Request(origin + '/v1/admin/private-trial/upload', {
  method: 'POST',
  headers: { origin, 'content-length': String(length) },
  body: new Uint8Array(length),
});

test('private trial requires every staging arm before reading its bounded body', async () => {
  const key = Buffer.alloc(32, 31).toString('base64url');
  const env = {
    HYPERDRIVE_CACHING_DISABLED: '1', RP_ID: 'staging.nntune.com', RP_ORIGIN: origin,
    SESSION_SECRET_BASE64: key, VAULT_KEK_BASE64: key, VAULT_KEK_VERSION: '1',
    SUBSONIC_MEASUREMENT_STAGE: 'staging', PRIVATE_TRIAL_UPLOAD_ENABLED: 'true',
    PRIVATE_PLAYBACK_ENABLED: 'true',
    PRIVATE_TRIAL_UPLOAD_TARGET: JSON.stringify({
      invite_id: 1, job_id: 'synthetic', session_id: '', object_id: '', generation_id: '',
      map_id: '', credential_id: 'fixture', title: 'Synthetic', release_mbid: '',
    }),
  };
  for (const flag of ['SUBSONIC_MEASUREMENT_STAGE', 'PRIVATE_TRIAL_UPLOAD_ENABLED',
    'PRIVATE_PLAYBACK_ENABLED', 'PRIVATE_TRIAL_UPLOAD_TARGET']) {
    const unarmed = request(byteLimit);
    const without = { ...env };
    delete without[flag];
    assert.equal((await dispatch(unarmed, without)).status, 404, flag);
    assert.equal(unarmed.bodyUsed, false, flag);
  }

  const exact = request(byteLimit);
  assert.equal((await dispatch(exact, env)).status, 404, 'missing cohort');
  assert.equal(exact.bodyUsed, true);

  const over = request(byteLimit + 1);
  assert.equal((await dispatch(over, env)).status, 400);
  assert.equal(over.bodyUsed, false);
});

test('the database identity probe requires the disarmed mode, exact staging URL and runner bearer', async () => {
  const token = Buffer.alloc(32, 45).toString('base64url');
  const now = Math.floor(Date.now() / 1000);
  const env = {
    QA_CODE_SEAM: '1',
    PRIVATE_TRIAL_IDENTITY_MODE: 'all',
    PRIVATE_TRIAL_IDENTITY_RUNNER: `v1:${now - 60}:${now + 60}:${token}`,
  };
  const endpoint = '/v1/admin/private-trial/db-identity';
  const call = (overrides = {}, method = 'POST', bearer = token, url = origin + endpoint) =>
    dispatch(new Request(url, { method, headers: { authorization: `Bearer ${bearer}` } }), { ...env, ...overrides });

  assert.equal((await call({ PRIVATE_TRIAL_IDENTITY_MODE: '' })).status, 404);
  assert.equal((await call({ PRIVATE_TRIAL_UPLOAD_ENABLED: 'true' })).status, 404);
  assert.equal((await call({ PRIVATE_PLAYBACK_ENABLED: 'true' })).status, 404);
  assert.equal((await call({}, 'GET')).status, 404);
  assert.equal((await call({}, 'POST', 'a'.repeat(43))).status, 404);
  assert.equal((await call({}, 'POST', token, 'https://other.example.test' + endpoint)).status, 404);
  const admitted = await call();
  assert.equal(admitted.status, 503, 'admitted requests reach the missing binding');
  assert.equal(admitted.headers.get('cache-control'), 'no-store');
});
