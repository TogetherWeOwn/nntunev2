'use strict';

const assert = require('node:assert/strict');
const { test } = require('node:test');
const { connect } = require('../../../ingest/tests/fixtures/catalogue_postgres_socket.cjs');
const loadWorker = require('../../../ingest/tests/fixtures/worker_fixture_wasm.cjs');
const { private_trial_dispatch_test: dispatch } = loadWorker(process.env.PRIVATE_TRIAL_DISPATCH_WASM_JS);

const origin = 'https://staging.nntune.com';
const byteLimit = 177175;
const request = (length) => new Request(origin + '/v1/admin/private-trial/upload', {
  method: 'POST',
  headers: { origin, 'content-length': String(length) },
  body: new Uint8Array(length),
});
const identityUrl = origin + '/v1/admin/private-trial/db-identity';
const identityToken = Buffer.alloc(32, 45).toString('base64url');
const identityRequest = (method = 'POST', bearer = identityToken, url = identityUrl) =>
  new Request(url, { method, headers: { authorization: `Bearer ${bearer}` } });

function identityEnv(mode) {
  const now = Math.floor(Date.now() / 1000);
  return {
    QA_CODE_SEAM: '1',
    PRIVATE_TRIAL_IDENTITY_MODE: mode,
    PRIVATE_TRIAL_IDENTITY_RUNNER: `v1:${now - 60}:${now + 60}:${identityToken}`,
  };
}

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
  const env = identityEnv('all');
  const call = (overrides = {}, method, bearer, url) =>
    dispatch(identityRequest(method, bearer, url), { ...env, ...overrides });

  assert.equal((await call({ QA_CODE_SEAM: '' })).status, 404);
  assert.equal((await call({ PRIVATE_TRIAL_IDENTITY_MODE: '' })).status, 404);
  assert.equal((await call({ PRIVATE_TRIAL_UPLOAD_ENABLED: 'true' })).status, 404);
  assert.equal((await call({ PRIVATE_PLAYBACK_ENABLED: 'true' })).status, 404);
  assert.equal((await call({}, 'GET')).status, 404);
  const unauthorized = await call({}, 'POST', 'a'.repeat(43));
  assert.equal(unauthorized.status, 404);
  assert.equal(Object.hasOwn(await unauthorized.json(), 'diagnostic'), false);
  assert.equal((await call({}, 'POST', identityToken, identityUrl.replace(origin, 'https://other.example.test'))).status, 404);
  const admitted = await call();
  assert.equal(admitted.status, 503);
  assert.equal(admitted.headers.get('cache-control'), 'no-store');
  assert.deepEqual((await admitted.json()).diagnostic, {
    binding: null, phase: 'version_metadata', error_class: 'missing_or_invalid_binding', sqlstate: null,
  });
});

test('the protected upload probe diagnoses the native Hyperdrive socket and startup', async (t) => {
  const logs = t.mock.method(console, 'log', () => {});
  const binding = 'PRIVATE_TRIAL_UPLOAD_DB';
  const role = 'synthetic_worker_role';
  const privateDetail = 'synthetic-only-postgres-error-detail';
  const sha = '1'.repeat(40);
  const env = {
    ...identityEnv('upload'),
    BUILD_SHA: sha,
    PRIVATE_TRIAL_IDENTITY_VERSION: { id: 'test-version' },
  };
  const call = async (phase, error_class, sqlstate = null) => {
    const response = await dispatch(identityRequest(), env);
    assert.equal(response.status, 503);
    const diagnostic = { binding, phase, error_class, sqlstate };
    assert.deepEqual(await response.json(), {
      code: 'service_unavailable', message: 'service unavailable', retryable: true,
      build_sha: sha, diagnostic,
    });
    const line = JSON.parse(logs.mock.calls.at(-1).arguments[0]);
    assert.equal(line.event, 'api.private_trial.identity_failed');
    assert.deepEqual(line.fields, { ...diagnostic, build_sha: sha });
  };

  await call('role_env', 'missing_or_invalid_binding');
  env[binding + '_ROLE'] = role;
  await call('hyperdrive_binding', 'missing_or_invalid_binding');

  let startup;
  let connectionAttempts = 0;
  class Hyperdrive {
    user = 'edge_user';
    database = 'edge_db';
    password = 'synthetic-only-password-with-@:[]';
    connect() {
      if (++connectionAttempts === 1) throw new Error(privateDetail);
      const error = Buffer.from(`SFATAL\0C28P01\0M${privateDetail}\0\0`);
      const frame = Buffer.alloc(5 + error.length);
      frame[0] = 'E'.charCodeAt(0);
      frame.writeUInt32BE(error.length + 4, 1);
      error.copy(frame, 5);
      const readable = new ReadableStream({
        start(controller) { controller.enqueue(frame); controller.close(); },
      });
      return {
        opened: Promise.resolve({}),
        readable,
        writable: new WritableStream({ write(chunk) { startup = Buffer.from(chunk); } }),
      };
    }
  }
  const target = env[binding] = new Hyperdrive();
  target.database = '';
  await call('binding_identity', 'invalid_binding_database');
  target.database = 'edge_db';
  target.user = undefined;
  await call('binding_identity', 'invalid_binding_user');
  assert.equal(connectionAttempts, 0);
  target.user = 'edge_user';
  await call('edge_connect', 'socket');
  await call('postgres_startup_auth', 'authentication', '28P01');
  assert.equal(startup.readUInt32BE(4), 196608, 'send a PostgreSQL startup frame on the edge socket');
  assert.ok(startup.includes(Buffer.from('user\0edge_user\0database\0edge_db\0')));
});

test('the native Hyperdrive socket returns the disposable PostgreSQL identity', async (t) => {
  const binding = 'PRIVATE_TRIAL_UPLOAD_DB';
  const user = 'trial_identity_fixture';
  const database = 'nntunev2_staging';
  const pins = process.env.TRIAL_IDENTITY_OIDS ?? '';
  assert.match(pins, /^[1-9][0-9]*\|[1-9][0-9]*$/);
  const [databaseOid, roleOid] = pins.split('|').map(Number);
  assert.notEqual(databaseOid, roleOid);
  const sha = '1'.repeat(40);
  const version = 'test-version';
  class Hyperdrive {
    user = 'edge_user';
    database = 'edge_db';
    password = 'ci-throwaway';

    constructor(originDatabase, test) {
      this.originDatabase = originDatabase;
      this.test = test;
    }

    connect() {
      const socket = connect({ hostname: '127.0.0.1', port: 5432 }, { secureTransport: 'off' });
      this.test.after(() => socket.close());
      const writer = socket.writable.getWriter();
      let started = false;
      const writable = new WritableStream({
        write: (chunk) => {
          if (started) return writer.write(chunk);
          started = true;
          // Model the edge mapping startup credentials to the configured origin.
          const frame = Buffer.from(chunk);
          assert.equal(frame.readUInt32BE(4), 196608);
          const edge = 'user\0edge_user\0database\0edge_db\0';
          const params = frame.subarray(8).toString();
          assert.ok(params.includes(edge));
          const mapped = Buffer.concat([frame.subarray(0, 8), Buffer.from(params.replace(
            edge, `user\0${user}\0database\0${this.originDatabase}\0`,
          ))]);
          mapped.writeUInt32BE(mapped.length, 0);
          return writer.write(mapped);
        },
      });
      return { ...socket, writable, opened: Promise.resolve({}) };
    }
  }
  const call = (test, originDatabase, pinnedUser) => dispatch(identityRequest(), {
    ...identityEnv('upload'),
    BUILD_SHA: sha,
    PRIVATE_TRIAL_IDENTITY_VERSION: { id: version },
    [binding + '_ROLE']: pinnedUser,
    [binding]: new Hyperdrive(originDatabase, test),
  });
  const response = await call(t, database, user);
  const body = await response.json();
  assert.equal(response.status, 200, JSON.stringify(body));
  assert.deepEqual(body, {
    build_sha: sha, version_id: version, rows: [[binding, [database, databaseOid, user, roleOid, user, roleOid]]],
  });
  for (const [originDatabase, pinnedUser, errorClass] of [
    ['postgres', user, 'database_mismatch'],
    [database, 'other_origin_role', 'user_mismatch'],
  ]) {
    await t.test(errorClass, async (subtest) => {
      const response = await call(subtest, originDatabase, pinnedUser);
      assert.equal(response.status, 503);
      assert.deepEqual(await response.json(), {
        code: 'service_unavailable', message: 'service unavailable', retryable: true,
        build_sha: sha,
        diagnostic: { binding, phase: 'role_query', error_class: errorClass, sqlstate: null },
      });
    });
  }
});
