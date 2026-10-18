-- Regression test for migration 0019. Run after
-- ALL migrations are applied (see ../run_migration_tests.sh). Everything
-- happens inside one transaction that is rolled back, so it leaves no rows;
-- any broken expectation RAISEs and (with ON_ERROR_STOP) fails the run.

BEGIN;

DO $$
BEGIN
    INSERT INTO users (id, email, created_at)
    VALUES ('user-nrc-1', 'nrc-1@example.com', 1700000000),
           ('user-nrc-2', 'nrc-2@example.com', 1700000000);

    -- A well-formed credential row stores (HMAC-only lookup identity,
    -- scopes, lifetime, listing prefix, audit IP) — never the secret.
    INSERT INTO native_rest_credentials
        (id, user_id, label, key_prefix, key_hmac, pepper_version,
         scopes, created_at, expires_at, created_from_ip)
    VALUES ('cred-1', 'user-nrc-1', 'cli', 'ntk_abcd1234',
            'aa11', 1, ARRAY['library:read', 'stream:read'],
            1700000000, 1731536000, '198.51.100.9');
    IF (SELECT count(*) FROM native_rest_credentials
        WHERE user_id = 'user-nrc-1' AND revoked_at IS NULL) <> 1 THEN
        RAISE EXCEPTION 'expected exactly one live row for user-nrc-1';
    END IF;

    -- Composite lookup identity: the same HMAC under a different pepper
    -- version is a different row (dual-version lookup during rotation).
    INSERT INTO native_rest_credentials
        (id, user_id, label, key_prefix, key_hmac, pepper_version,
         scopes, created_at, expires_at, created_from_ip)
    VALUES ('cred-2', 'user-nrc-1', 'rotated', 'ntk_abcd1234',
            'aa11', 2, ARRAY['library:read'],
            1700000000, 1731536000, '198.51.100.9');

    -- UNIQUE (pepper_version, key_hmac): the same HMAC twice under one
    -- version cannot exist (no duplicate lookup identities).
    BEGIN
        INSERT INTO native_rest_credentials
            (id, user_id, label, key_prefix, key_hmac, pepper_version,
             scopes, created_at, expires_at, created_from_ip)
        VALUES ('cred-dup', 'user-nrc-2', 'dup', 'ntk_abcd1234',
                'aa11', 1, ARRAY['library:read'],
                1700000000, 1731536000, '198.51.100.9');
        RAISE EXCEPTION 'UNIQUE accepted a duplicate (pepper_version, key_hmac)';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;

    -- Label length fence: over-long labels cannot be inserted.
    BEGIN
        INSERT INTO native_rest_credentials
            (id, user_id, label, key_prefix, key_hmac, pepper_version,
             scopes, created_at, expires_at, created_from_ip)
        VALUES ('cred-long', 'user-nrc-1', repeat('l', 65), 'ntk_abcd1234',
                'bb22', 1, ARRAY['library:read'],
                1700000000, 1731536000, '198.51.100.9');
        RAISE EXCEPTION 'CHECK accepted an over-long label';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Owner fence: credentials cannot point at a missing user.
    BEGIN
        INSERT INTO native_rest_credentials
            (id, user_id, label, key_prefix, key_hmac, pepper_version,
             scopes, created_at, expires_at, created_from_ip)
        VALUES ('cred-orphan', 'user-missing', '', 'ntk_abcd1234',
                'cc33', 1, ARRAY['library:read'],
                1700000000, 1731536000, '198.51.100.9');
        RAISE EXCEPTION 'FK accepted a credential for a missing user';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;
    END;

    -- Deletion fence (ADR 0002 rule 3): the production mint writes
    -- through INSERT ... SELECT over the owner fence
    -- (worker_entry.rs insert_native_credential), so a deleting account's
    -- insert lands zero rows and fails closed. The fence is application
    -- SQL, not a table constraint, so replicate the fenced shape here: a
    -- direct VALUES insert would bypass it and must not be used for this
    -- case.
    INSERT INTO users (id, email, created_at)
    VALUES ('user-nrc-3', 'nrc-3@example.com', 1700000000);
    UPDATE users SET deleting_at = NOW() WHERE id = 'user-nrc-3';
    INSERT INTO native_rest_credentials
        (id, user_id, label, key_prefix, key_hmac, pepper_version,
         scopes, created_at, expires_at, created_from_ip)
    SELECT 'cred-deleting', 'user-nrc-3', '', 'ntk_abcd1234',
           'dd44', 1, ARRAY['library:read'],
           1700000000, 1731536000, '198.51.100.9'
    FROM (SELECT id FROM users
          WHERE id = 'user-nrc-3' AND deleting_at IS NULL FOR SHARE) AS writer_fence;
    IF (SELECT count(*) FROM native_rest_credentials
        WHERE id = 'cred-deleting') <> 0 THEN
        RAISE EXCEPTION 'fenced insert landed a row behind the deletion marker';
    END IF;
    -- Positive control: the same fenced shape lands one row for a live user.
    INSERT INTO native_rest_credentials
        (id, user_id, label, key_prefix, key_hmac, pepper_version,
         scopes, created_at, expires_at, created_from_ip)
    SELECT 'cred-fenced-live', 'user-nrc-2', '', 'ntk_abcd1234',
           'ee55', 1, ARRAY['library:read'],
           1700000000, 1731536000, '198.51.100.9'
    FROM (SELECT id FROM users
          WHERE id = 'user-nrc-2' AND deleting_at IS NULL FOR SHARE) AS writer_fence;
    IF (SELECT count(*) FROM native_rest_credentials
        WHERE id = 'cred-fenced-live') <> 1 THEN
        RAISE EXCEPTION 'fenced insert missed a live user';
    END IF;

    -- Erasure cascade: deleting the user removes its credential rows
    -- (child order for account erasure, alongside sessions).
    DELETE FROM users WHERE id = 'user-nrc-1';
    IF (SELECT count(*) FROM native_rest_credentials
        WHERE user_id = 'user-nrc-1') <> 0 THEN
        RAISE EXCEPTION 'user delete left orphan credential rows';
    END IF;
END $$;

ROLLBACK;
