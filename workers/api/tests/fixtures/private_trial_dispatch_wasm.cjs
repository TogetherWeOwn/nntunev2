'use strict';

const assert = require('node:assert/strict');
const { test } = require('node:test');
const { private_trial_dispatch_test: dispatch } = require(process.env.STAGING_BODY_WASM_JS);

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
  assert.equal((await dispatch(exact, env)).status, 404, 'anonymous request');
  assert.equal(exact.bodyUsed, true);

  const over = request(byteLimit + 1);
  assert.equal((await dispatch(over, env)).status, 400);
  assert.equal(over.bodyUsed, false);
});
