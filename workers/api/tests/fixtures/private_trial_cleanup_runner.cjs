'use strict';

const assert = require('node:assert/strict');
const { execFileSync } = require('node:child_process');
const { createHash } = require('node:crypto');
const { join } = require('node:path');
const { test } = require('node:test');
const { invoke } = require('../../../../.github/scripts/staging_trial_cleanup.cjs');

const db = 'nntunev2_staging';
const role = 'trial_cleanup_fixture';
const uri = `postgresql://${role}:disposable-cleanup-password@127.0.0.1:5432/${db}?sslmode=require`;
const sha = (raw) => createHash('sha256').update(raw).digest('hex');
const id = (n) => `10000000-0000-4000-8000-00000000000${n}`;
const keyId = (uuid) => uuid.replaceAll('-', '');
const provider = {
  environment: id(1), library: id(2), configuration: id(3), root_version: id(4),
  bucket: 'nntunev2-private-fixtures-staging', endpoint: 'https://cleanup-s3.example.test', region: 'fixture-region',
};
const cleanupAccessKey = 'FIXTUREACCESS';
const s3 = JSON.stringify({ access_key: cleanupAccessKey, secret_key: 'synthetic-version-secret' });
const preflightS3 = JSON.stringify({ access_key: 'PREFLIGHTACCESS', secret_key: 'synthetic-preflight-secret' });
const scope = {
  provider, database_host: '127.0.0.1', database_port: 5432, database_role: role, s3_credential_sha256: sha(s3),
  object: id(5), generation: id(6), map: id(7), track: id(8), session: id(9),
  job: 'cleanup-fixture-job', issuer: 'cleanup-issuer', invite: 1931,
  media_version: 'media/selected+version', page_version: 'page?selected&version',
  cutoff: Math.floor(Date.now() / 1000) - 30,
};
const mediaKey = `media/${keyId(scope.object)}/${keyId(scope.generation)}`;
const pageKey = `media-digests/${keyId(scope.object)}/${keyId(scope.generation)}/${keyId(scope.map)}/00000000`;
const siblingKey = `media/${keyId(id('b'))}/${keyId(id('c'))}`;
const psql = (database, ...args) => execFileSync('psql', [
  '-XqAt', '-h', '127.0.0.1', '-p', '5432', '-U', 'postgres', '-d', database,
  '-v', 'ON_ERROR_STOP=1', ...args,
], { env: { PATH: process.env.PATH, PGPASSWORD: 'ci-throwaway' }, encoding: 'utf8', timeout: 15_000 }).trim();

test('refuses foreign fixture pins, environment and authority and recovers the same fixture', { timeout: 120_000 }, async () => {
  assert.equal(process.env.GITHUB_ACTIONS, 'true');
  assert.equal(process.env.RUNNER_ENVIRONMENT, 'github-hosted');
  assert.ok(process.env.PRIVATE_TRIAL_CLEANUP_BUNDLE);
  // The Worker release job clones the migrated schema before the upload fixture runs.
  try {
    const vars = {
      env: provider.environment, lib: provider.library, root: provider.root_version, config: provider.configuration,
      object: scope.object, generation: scope.generation, map: scope.map, track: scope.track, session: scope.session,
      bucket: provider.bucket, media_key: mediaKey, media_version: scope.media_version, page_version: scope.page_version,
    };
    psql(db, ...Object.entries(vars).flatMap(([k, v]) => ['-v', `${k}=${v}`]),
      '-f', join(__dirname, 'private_trial_cleanup_runner.sql'));
    scope.database_role_oid = Number(psql(db, '-c', "SELECT oid FROM pg_roles WHERE rolname = 'trial_cleanup_fixture'"));
    assert.ok(scope.database_role_oid > 0);
    const state = () => psql(db, '-c', `SELECT
      (SELECT deleting_at IS NOT NULL FROM public.storage_validated_digest_winners WHERE object_id = '${scope.object}'),
      (SELECT count(*) FROM public.private_library_admissions),
      (SELECT count(*) FROM public.storage_validated_digest_references WHERE object_id = '${scope.object}'),
      (SELECT count(*) FROM public.catalogue_s3_sources)`);
    const versions = new Map([[mediaKey, new Set([scope.media_version, 'other-media'])],
      [pageKey, new Set([scope.page_version, 'other-page'])], [siblingKey, new Set(['sibling-version'])]]);
    const calls = [];
    let interrupt = true;
    const origin = async (req) => {
      const url = new URL(req.url);
      assert.equal(url.origin, provider.endpoint);
      const prefix = `/${provider.bucket}/`;
      assert.ok(url.pathname.startsWith(prefix));
      const key = url.pathname.slice(prefix.length);
      const version = url.searchParams.get('versionId');
      assert.deepEqual([...url.searchParams], [['versionId', key === mediaKey ? scope.media_version : scope.page_version]]);
      assert.match(req.headers.get('authorization'), /Credential=FIXTUREACCESS\//);
      calls.push([req.method, key, version]);
      assert.ok(versions.has(key));
      if (req.method === 'DELETE') {
        versions.get(key).delete(version);
        return new Response(null, { status: 204 });
      }
      assert.equal(req.method, 'HEAD');
      if (key === mediaKey && interrupt) return new Response(null, { status: 503 });
      return new Response(null, { status: versions.get(key).has(version) ? 200 : 404 });
    };
    const approvalFor = (selection) => ({
      provider, scope_sha256: sha(JSON.stringify(selection)), s3_access_key_sha256: sha(cleanupAccessKey),
    });
    const run = (selection = scope, overrides = {}) => {
      const raw = JSON.stringify(selection);
      return invoke(process.env.PRIVATE_TRIAL_CLEANUP_BUNDLE, {
        CLEANUP_STAGE: 'staging', CLEANUP_SCOPE: raw, CLEANUP_SCOPE_SHA256: sha(raw),
        CLEANUP_APPROVAL: JSON.stringify(approvalFor(scope)), CLEANUP_S3: s3, CLEANUP_DATABASE_URL: uri, ...overrides,
      }, origin);
    };
    const approved = (selection) => JSON.stringify(approvalFor(selection));
    const wrongVersion = { ...scope, page_version: 'other-page' };
    const wrongFixture = { ...scope, track: id('a') };
    const wrongCutoff = { ...scope, cutoff: scope.cutoff + 3600 };
    const wrongRole = { ...scope, database_role_oid: scope.database_role_oid + 1 };
    const controls = [
      ['missing invocation scope pin', scope, { CLEANUP_SCOPE_SHA256: '' }],
      ['wrong environment', scope, { CLEANUP_STAGE: 'production' }],
      ['wrong trusted provider', scope, { CLEANUP_APPROVAL: JSON.stringify({
        ...approvalFor(scope), provider: { ...provider, endpoint: 'https://foreign.example.test' },
      }) }],
      ['wrong recorded version', wrongVersion, { CLEANUP_APPROVAL: approved(wrongVersion) }],
      ['wrong recorded fixture', wrongFixture, { CLEANUP_APPROVAL: approved(wrongFixture) }],
      ['wrong DB identity', wrongRole, { CLEANUP_APPROVAL: approved(wrongRole) }],
      ['wrong DB target', scope, { CLEANUP_DATABASE_URL: uri.replace(db, 'nntune_ci') }],
      ['plaintext database connection', scope, { CLEANUP_DATABASE_URL: uri.replace('sslmode=require', 'sslmode=disable') }],
      ['wrong S3 authority', scope, { CLEANUP_S3: s3.replace('synthetic-version-secret', 'other-secret') }],
      ['before cohort cutoff', wrongCutoff, { CLEANUP_APPROVAL: approved(wrongCutoff) }],
    ];
    for (const [label, selection, overrides] of controls) {
      assert.equal(await run(selection, overrides), 'refused', label);
    }
    assert.equal(await run(), 'withheld', 'retention must withhold retirement');
    assert.equal(state(), 'f|1|1|1');
    assert.deepEqual(calls, []);

    psql(db, '-c', `UPDATE public.storage_validated_digest_winners SET retain_until = '-infinity'
      WHERE object_id = '${scope.object}'`);
    const attempt = [['DELETE', mediaKey, scope.media_version], ['HEAD', mediaKey, scope.media_version],
      ['DELETE', pageKey, scope.page_version], ['HEAD', pageKey, scope.page_version]];
    const partial = attempt.slice(0, 2);
    assert.equal(await run(), 'unconfirmed');
    assert.deepEqual(calls, partial);
    assert.equal(state(), 't|0|0|1');
    assert.equal(versions.get(mediaKey).has(scope.media_version), false);
    assert.equal(versions.get(pageKey).has(scope.page_version), true);
    const preflightScope = { ...scope, s3_credential_sha256: sha(preflightS3) };
    const continuationControls = [
      ['another upload attempt', { ...scope, session: id('a'), job: 'other-attempt' }],
      ['another fixture', wrongFixture],
      ['another version', wrongVersion],
      ['changed scope bytes with the original dispatch pin', wrongVersion, {
        CLEANUP_SCOPE_SHA256: approvalFor(scope).scope_sha256,
      }],
      ['missing scope', scope, { CLEANUP_SCOPE: '' }],
      ['missing approval', scope, { CLEANUP_APPROVAL: '' }],
      ['missing approved scope pin', scope, { CLEANUP_APPROVAL: JSON.stringify({ ...approvalFor(scope), scope_sha256: undefined }) }],
      ['missing approved key pin', scope, { CLEANUP_APPROVAL: JSON.stringify({ ...approvalFor(scope), s3_access_key_sha256: undefined }) }],
      ['storage preflight identity even with matching credential JSON pin', preflightScope, {
        CLEANUP_APPROVAL: approved(preflightScope), CLEANUP_S3: preflightS3,
      }],
    ];
    for (const [label, selection, overrides] of continuationControls) {
      assert.equal(await run(selection, overrides), 'refused', label);
      assert.deepEqual(calls, partial, label);
      assert.equal(state(), 't|0|0|1', label);
    }

    interrupt = false;
    assert.equal(await run(), 'confirmed');
    assert.deepEqual(calls, [...partial, ...attempt]);
    assert.equal(state(), 't|0|0|0');
    assert.deepEqual([...versions.get(mediaKey)], ['other-media']);
    assert.deepEqual([...versions.get(pageKey)], ['other-page']);
    assert.deepEqual([...versions.get(siblingKey)], ['sibling-version']);
  } finally {
    psql('postgres', '-c', 'DROP DATABASE IF EXISTS nntunev2_staging WITH (FORCE)');
    psql('postgres', '-c', 'DROP ROLE IF EXISTS trial_cleanup_fixture');
  }
});
