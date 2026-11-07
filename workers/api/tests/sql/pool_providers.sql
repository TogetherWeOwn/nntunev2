-- Regression test for migration 0036 (service pool provider registry).
-- Run after ALL migrations are applied (see ../run_migration_tests.sh,
-- which passes -v migrations_dir=...). One transaction, rolled back.
--
-- Part 1 rebuilds the pre-0036 shape of user_cache_grants (composite
-- (user_id, provider_id) FK to providers, provider_id NOT NULL), then
-- re-executes the real migration file and proves the old attribution is
-- moved to legacy_provider_id, not lost. Part 2 proves a re-apply does
-- not touch NEW pool attribution. Part 3 pins the pool schema checks.

BEGIN;

INSERT INTO users (id, email, created_at)
VALUES ('user-pp-1', 'pp-1@example.com', 1700000000),
       ('user-pp-2', 'pp-2@example.com', 1700000000);

INSERT INTO providers
    (id, user_id, label, encrypted_creds, creds_envelope_version, creds_kek_version,
     creds_dek_wrapped_nonce, creds_dek_wrapped, creds_payload_nonce, creds_payload_format,
     created_at, last_verified_at)
VALUES ('aaaaaaaa-0000-4000-8000-000000000001', 'user-pp-1', 'legacy-1', '\x0102'::bytea, 2, 1,
        decode(repeat('00', 12), 'hex'), decode(repeat('00', 48), 'hex'),
        decode(repeat('00', 12), 'hex'), 1, 1700000000, 1700000000);

INSERT INTO cache_objects (content_hash, byte_size, r2_bucket, r2_key, last_verified_at)
VALUES (repeat('a', 64), 1, 'b', 'k/a', 1700000000),
       (repeat('b', 64), 1, 'b', 'k/b', 1700000000),
       (repeat('c', 64), 1, 'b', 'k/c', 1700000000);

-- Part 1: rebuild the pre-0036 state with two legacy-attributed grants.
INSERT INTO user_cache_grants (user_id, content_hash, legacy_provider_id, verified_at)
VALUES ('user-pp-1', repeat('a', 64), 'aaaaaaaa-0000-4000-8000-000000000001', 1700000000),
       ('user-pp-1', repeat('b', 64), 'aaaaaaaa-0000-4000-8000-000000000001', 1700000000);

ALTER TABLE user_cache_grants DROP CONSTRAINT user_cache_grants_pool_provider_fkey;
ALTER TABLE user_cache_grants DROP CONSTRAINT user_cache_grants_legacy_provider_fkey;
UPDATE user_cache_grants SET provider_id = legacy_provider_id, legacy_provider_id = NULL;
ALTER TABLE user_cache_grants DROP COLUMN legacy_provider_id;
ALTER TABLE user_cache_grants ALTER COLUMN provider_id SET NOT NULL;
ALTER TABLE user_cache_grants
    ADD CONSTRAINT user_cache_grants_user_id_provider_id_fkey
    FOREIGN KEY (user_id, provider_id) REFERENCES providers (user_id, id) ON DELETE RESTRICT;

\i :migrations_dir/0036_pool_providers.sql

DO $$
BEGIN
    IF (SELECT count(*) FROM user_cache_grants) <> 2 THEN
        RAISE EXCEPTION 'migration lost grant rows';
    END IF;
    IF (SELECT count(*) FROM user_cache_grants
        WHERE legacy_provider_id = 'aaaaaaaa-0000-4000-8000-000000000001') <> 2 THEN
        RAISE EXCEPTION 'legacy attribution was not preserved in legacy_provider_id';
    END IF;
    IF (SELECT count(*) FROM user_cache_grants WHERE provider_id IS NOT NULL) <> 0 THEN
        RAISE EXCEPTION 'provider_id was not cleared after the copy';
    END IF;
    IF (SELECT data_type FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'user_cache_grants'
          AND column_name = 'provider_id') <> 'uuid' THEN
        RAISE EXCEPTION 'provider_id must stay uuid';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_constraint
               WHERE conrelid = 'public.user_cache_grants'::regclass
                 AND conname = 'user_cache_grants_user_id_provider_id_fkey') THEN
        RAISE EXCEPTION 'old composite FK survived';
    END IF;
    IF (SELECT count(*) FROM pg_constraint
        WHERE conrelid = 'public.user_cache_grants'::regclass
          AND conname IN ('user_cache_grants_legacy_provider_fkey',
                          'user_cache_grants_pool_provider_fkey')) <> 2 THEN
        RAISE EXCEPTION 'legacy and pool FKs must both exist';
    END IF;
    -- The legacy providers row stays undeletable while it is the only
    -- copy of the old attribution.
    BEGIN
        DELETE FROM providers WHERE id = 'aaaaaaaa-0000-4000-8000-000000000001';
        RAISE EXCEPTION 'legacy provider deletable despite legacy attribution';
    EXCEPTION WHEN foreign_key_violation OR restrict_violation THEN
        NULL;
    END;
    -- The composite FK still binds the legacy provider to its own user.
    BEGIN
        INSERT INTO user_cache_grants (user_id, content_hash, legacy_provider_id, verified_at)
        VALUES ('user-pp-2', repeat('c', 64), 'aaaaaaaa-0000-4000-8000-000000000001', 1700000000);
        RAISE EXCEPTION 'legacy FK accepted another user''s provider';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;
    END;
END $$;

-- Part 2: new pool attribution survives a re-apply.
INSERT INTO pool_provider_secrets
    (secret_ref, encrypted_creds, creds_envelope_version, creds_kek_version,
     creds_dek_wrapped_nonce, creds_dek_wrapped, creds_payload_nonce, creds_payload_format,
     created_at, updated_at)
VALUES ('bbbbbbbb-0000-4000-8000-000000000001', '\x0102'::bytea, 2, 1,
        decode(repeat('00', 12), 'hex'), decode(repeat('00', 48), 'hex'),
        decode(repeat('00', 12), 'hex'), 1, 1700000000, 1700000000);

INSERT INTO pool_providers
    (id, label, host, port, tls, username, can_read, can_post, max_connections,
     stream_reserve, ingest_limit, backbone, tier, enabled, secret_ref, created_at, updated_at)
VALUES ('cccccccc-0000-4000-8000-000000000001', 'Primary', 'news.example.com', 563, true,
        'svc', true, false, 20, 5, 15, 'omicron', 1, true,
        'bbbbbbbb-0000-4000-8000-000000000001', 1700000000, 1700000000);

UPDATE user_cache_grants SET provider_id = 'cccccccc-0000-4000-8000-000000000001'
WHERE user_id = 'user-pp-1' AND content_hash = repeat('a', 64);

\i :migrations_dir/0036_pool_providers.sql

DO $$
BEGIN
    IF (SELECT count(*) FROM user_cache_grants
        WHERE provider_id = 'cccccccc-0000-4000-8000-000000000001'
          AND content_hash = repeat('a', 64)) <> 1 THEN
        RAISE EXCEPTION 're-apply cleared NEW pool attribution';
    END IF;
    IF (SELECT count(*) FROM user_cache_grants WHERE provider_id IS NULL) <> 1 THEN
        RAISE EXCEPTION 're-apply changed unattributed grants';
    END IF;
    IF (SELECT count(*) FROM user_cache_grants
        WHERE legacy_provider_id = 'aaaaaaaa-0000-4000-8000-000000000001') <> 2 THEN
        RAISE EXCEPTION 're-apply disturbed legacy attribution';
    END IF;
    IF (SELECT revision FROM pool_providers) <> 1 THEN
        RAISE EXCEPTION 'default revision must be 1';
    END IF;
END $$;

-- Part 3: pool schema checks.
DO $$
DECLARE
    base_sql CONSTANT TEXT :=
        'INSERT INTO pool_providers (id, label, host, port, tls, username, can_read, can_post, '
        'max_connections, stream_reserve, ingest_limit, backbone, tier, enabled, secret_ref, '
        'created_at, updated_at) VALUES (%L, ''L'', ''h.example.com'', %s, %L, ''u'', %L, false, '
        '%s, %s, %s, ''bb'', %s, true, %L, 1700000000, 1700000000)';
    sref CONSTANT TEXT := 'bbbbbbbb-0000-4000-8000-000000000001';
    pid  CONSTANT TEXT := 'cccccccc-0000-4000-8000-000000000002';
BEGIN
    -- control: a valid row inserts.
    EXECUTE format(base_sql, pid, 563, true, true, 10, 2, 8, 1, sref);
    DELETE FROM pool_providers WHERE id = pid::uuid;

    -- tls required, port allowlist, read/ingest coupling, bounds.
    BEGIN EXECUTE format(base_sql, pid, 563, false, true, 10, 2, 8, 1, sref);
        RAISE EXCEPTION 'accepted tls=false'; EXCEPTION WHEN check_violation THEN NULL; END;
    BEGIN EXECUTE format(base_sql, pid, 8080, true, true, 10, 2, 8, 1, sref);
        RAISE EXCEPTION 'accepted off-allowlist port'; EXCEPTION WHEN check_violation THEN NULL; END;
    BEGIN EXECUTE format(base_sql, pid, 563, true, true, 51, 2, 8, 1, sref);
        RAISE EXCEPTION 'accepted max_connections 51'; EXCEPTION WHEN check_violation THEN NULL; END;
    BEGIN EXECUTE format(base_sql, pid, 563, true, true, 10, 0, 0, 1, sref);
        RAISE EXCEPTION 'accepted read account with stream_reserve 0'; EXCEPTION WHEN check_violation THEN NULL; END;
    BEGIN EXECUTE format(base_sql, pid, 563, true, true, 10, 2, 9, 1, sref);
        RAISE EXCEPTION 'accepted ingest_limit > max - reserve'; EXCEPTION WHEN check_violation THEN NULL; END;
    BEGIN EXECUTE format(base_sql, pid, 563, true, false, 10, 0, 1, 1, sref);
        RAISE EXCEPTION 'accepted ingest on non-read account'; EXCEPTION WHEN check_violation THEN NULL; END;
    BEGIN EXECUTE format(base_sql, pid, 563, true, true, 10, 2, 8, 101, sref);
        RAISE EXCEPTION 'accepted tier 101'; EXCEPTION WHEN check_violation THEN NULL; END;
    BEGIN EXECUTE format(base_sql, pid, 563, true, true, 10, 2, 8, 1, 'dddddddd-0000-4000-8000-000000000009');
        RAISE EXCEPTION 'accepted missing secret'; EXCEPTION WHEN foreign_key_violation THEN NULL; END;

    -- revision must stay positive.
    BEGIN UPDATE pool_providers SET revision = 0;
        RAISE EXCEPTION 'accepted revision 0'; EXCEPTION WHEN check_violation THEN NULL; END;

    -- secret_ref shape and envelope guards.
    BEGIN
        INSERT INTO pool_provider_secrets
            (secret_ref, encrypted_creds, creds_envelope_version, creds_kek_version,
             creds_dek_wrapped_nonce, creds_dek_wrapped, creds_payload_nonce,
             creds_payload_format, created_at, updated_at)
        VALUES ('not-a-uuid', '\x01'::bytea, 2, 1, decode(repeat('00', 12), 'hex'),
                decode(repeat('00', 48), 'hex'), decode(repeat('00', 12), 'hex'), 1, 1, 1);
        RAISE EXCEPTION 'accepted malformed secret_ref';
    EXCEPTION WHEN check_violation THEN NULL; END;
    BEGIN
        INSERT INTO pool_provider_secrets
            (secret_ref, encrypted_creds, creds_envelope_version, creds_kek_version,
             creds_dek_wrapped_nonce, creds_dek_wrapped, creds_payload_nonce,
             creds_payload_format, created_at, updated_at)
        VALUES ('bbbbbbbb-0000-4000-8000-000000000002', '\x01'::bytea, 1, 1,
                decode(repeat('00', 12), 'hex'), decode(repeat('00', 48), 'hex'),
                decode(repeat('00', 12), 'hex'), 1, 1, 1);
        RAISE EXCEPTION 'accepted non-v2 envelope';
    EXCEPTION WHEN check_violation THEN NULL; END;

    -- RESTRICT: referenced rows cannot be deleted.
    BEGIN DELETE FROM pool_provider_secrets;
        RAISE EXCEPTION 'secret deletable while referenced';
    EXCEPTION WHEN foreign_key_violation OR restrict_violation THEN NULL; END;
    BEGIN DELETE FROM pool_providers;
        RAISE EXCEPTION 'pool provider deletable while a grant references it';
    EXCEPTION WHEN foreign_key_violation OR restrict_violation THEN NULL; END;

    -- Grant attribution must name a real pool provider.
    BEGIN
        UPDATE user_cache_grants SET provider_id = 'cccccccc-0000-4000-8000-0000000000ff'
        WHERE content_hash = repeat('b', 64);
        RAISE EXCEPTION 'accepted attribution to a missing pool provider';
    EXCEPTION WHEN foreign_key_violation THEN NULL; END;
END $$;

ROLLBACK;
