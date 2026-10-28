-- Regression test for the NEW API-key-only subsonic_credentials schema (not
-- the superseded 0023 legacy shape). Run after ALL migrations are applied
-- (see ../run_migration_tests.sh). Everything happens inside one transaction
-- that is rolled back, so it leaves no rows; any broken expectation RAISEs
-- and (with ON_ERROR_STOP) fails the run.
--
-- Scope: catalog shape (required columns, types, nullability, defaults, FK,
-- unique enforcements, legacy-material absence) plus constraint behavior on
-- synthetic fixtures: null/digest/version/auth_mode/scope rejections, duplicate
-- and orphan-FK rejections, same-digest-different-version allowed, and
-- independent per-row revocation for two clients of one owner.
--
-- Independently written from the accepted schema. It claims no
-- username/key issuer or generation authority (every value below is a
-- literal; the opaque `sn-client-*` names carry no PII and assert no
-- generation scheme), no runtime fence semantics, and it does not replicate
-- application SQL as proof of production behavior. The UPDATEs here only
-- exercise column nullability and row independence, not production
-- issuance/rotation/revocation flows.
--
-- NOT RUN here: this file is executed only by future CI via
-- run_migration_tests.sh against a scratch Postgres. DB calls and migration
-- execution are intentionally out of scope for this change; no psql or
-- migration was executed.

BEGIN;

DO $$
DECLARE
    hmac_a CONSTANT text := repeat('a', 64);
    hmac_b CONSTANT text := repeat('b', 64);
    hmac_c CONSTANT text := repeat('c', 64);
    hmac_d CONSTANT text := repeat('d', 64);
    got_default text;
    bad_auth_mode text;
    bad_scope text;
BEGIN
    -- Table exists.
    IF to_regclass('public.subsonic_credentials') IS NULL THEN
        RAISE EXCEPTION 'subsonic_credentials table is missing';
    END IF;

    -- Required columns with their types and nullability.
    PERFORM 1 FROM information_schema.columns
    WHERE table_schema = 'public' AND table_name = 'subsonic_credentials'
      AND column_name = 'id' AND data_type = 'uuid' AND is_nullable = 'NO';
    IF NOT FOUND THEN
        RAISE EXCEPTION 'subsonic_credentials.id must be UUID NOT NULL';
    END IF;
    -- id is the PRIMARY KEY (exactly that single column).
    IF (SELECT array_agg(kcu.column_name ORDER BY kcu.ordinal_position)
        FROM information_schema.table_constraints tc
        JOIN information_schema.key_column_usage kcu
          USING (constraint_catalog, constraint_schema, constraint_name)
        WHERE tc.table_schema = 'public'
          AND tc.table_name = 'subsonic_credentials'
          AND tc.constraint_type = 'PRIMARY KEY'
    ) IS DISTINCT FROM ARRAY['id'] THEN
        RAISE EXCEPTION 'subsonic_credentials.id must be the PRIMARY KEY';
    END IF;
    PERFORM 1 FROM information_schema.columns
    WHERE table_schema = 'public' AND table_name = 'subsonic_credentials'
      AND column_name = 'user_id' AND data_type = 'text' AND is_nullable = 'NO';
    IF NOT FOUND THEN
        RAISE EXCEPTION 'subsonic_credentials.user_id must be TEXT NOT NULL';
    END IF;
    PERFORM 1 FROM information_schema.columns
    WHERE table_schema = 'public' AND table_name = 'subsonic_credentials'
      AND column_name = 'label'
      AND data_type IN ('text', 'character varying', 'character')
      AND is_nullable = 'NO';
    IF NOT FOUND THEN
        RAISE EXCEPTION 'subsonic_credentials.label must be character NOT NULL';
    END IF;
    PERFORM 1 FROM information_schema.columns
    WHERE table_schema = 'public' AND table_name = 'subsonic_credentials'
      AND column_name = 'subsonic_username'
      AND data_type IN ('text', 'character varying', 'character')
      AND is_nullable = 'NO';
    IF NOT FOUND THEN
        RAISE EXCEPTION 'subsonic_credentials.subsonic_username must be character NOT NULL';
    END IF;
    PERFORM 1 FROM information_schema.columns
    WHERE table_schema = 'public' AND table_name = 'subsonic_credentials'
      AND column_name = 'api_key_hmac'
      AND data_type IN ('text', 'character varying', 'character')
      AND is_nullable = 'NO';
    IF NOT FOUND THEN
        RAISE EXCEPTION 'subsonic_credentials.api_key_hmac must be character NOT NULL';
    END IF;
    PERFORM 1 FROM information_schema.columns
    WHERE table_schema = 'public' AND table_name = 'subsonic_credentials'
      AND column_name = 'pepper_version' AND data_type = 'integer'
      AND is_nullable = 'NO';
    IF NOT FOUND THEN
        RAISE EXCEPTION 'subsonic_credentials.pepper_version must be INTEGER NOT NULL';
    END IF;
    PERFORM 1 FROM information_schema.columns
    WHERE table_schema = 'public' AND table_name = 'subsonic_credentials'
      AND column_name = 'auth_mode' AND is_nullable = 'NO';
    IF NOT FOUND THEN
        RAISE EXCEPTION 'subsonic_credentials.auth_mode must be NOT NULL';
    END IF;
    PERFORM 1 FROM information_schema.columns
    WHERE table_schema = 'public' AND table_name = 'subsonic_credentials'
      AND column_name = 'scope' AND is_nullable = 'NO';
    IF NOT FOUND THEN
        RAISE EXCEPTION 'subsonic_credentials.scope must be NOT NULL';
    END IF;
    PERFORM 1 FROM information_schema.columns
    WHERE table_schema = 'public' AND table_name = 'subsonic_credentials'
      AND column_name = 'created_at'
      AND data_type = 'timestamp with time zone' AND is_nullable = 'NO';
    IF NOT FOUND THEN
        RAISE EXCEPTION 'subsonic_credentials.created_at must be TIMESTAMPTZ NOT NULL';
    END IF;
    PERFORM 1 FROM information_schema.columns
    WHERE table_schema = 'public' AND table_name = 'subsonic_credentials'
      AND column_name = 'last_used_at'
      AND data_type = 'timestamp with time zone' AND is_nullable = 'YES';
    IF NOT FOUND THEN
        RAISE EXCEPTION 'subsonic_credentials.last_used_at must be nullable TIMESTAMPTZ';
    END IF;
    PERFORM 1 FROM information_schema.columns
    WHERE table_schema = 'public' AND table_name = 'subsonic_credentials'
      AND column_name = 'revoked_at'
      AND data_type = 'timestamp with time zone' AND is_nullable = 'YES';
    IF NOT FOUND THEN
        RAISE EXCEPTION 'subsonic_credentials.revoked_at must be nullable TIMESTAMPTZ';
    END IF;

    -- auth_mode/scope defaults (TEXT default or enum default; either pins the
    -- literal, so match the literal loosely).
    SELECT column_default INTO got_default FROM information_schema.columns
    WHERE table_schema = 'public' AND table_name = 'subsonic_credentials'
      AND column_name = 'auth_mode';
    IF got_default IS NULL OR got_default NOT LIKE '%apikey%' THEN
        RAISE EXCEPTION 'subsonic_credentials.auth_mode must default to apikey';
    END IF;
    SELECT column_default INTO got_default FROM information_schema.columns
    WHERE table_schema = 'public' AND table_name = 'subsonic_credentials'
      AND column_name = 'scope';
    IF got_default IS NULL OR got_default NOT LIKE '%subsonic%' THEN
        RAISE EXCEPTION 'subsonic_credentials.scope must default to subsonic';
    END IF;

    -- No legacy secret-material columns survive on the new table.
    IF EXISTS (SELECT 1 FROM information_schema.columns
               WHERE table_schema = 'public'
                 AND table_name = 'subsonic_credentials'
                 AND column_name = ANY (ARRAY['password', 'password_hash',
                    'password_enc', 'api_key', 'api_key_encrypted', 'token',
                    'token_hash', 'token_salt', 'secret', 'secret_encrypted',
                    'sealed_secret', 'sealed_password', 'legacy_secret',
                    'legacy_seal', 'token_envelope', 'key_version',
                    'ciphertext', 'nonce'])) THEN
        RAISE EXCEPTION 'subsonic_credentials carries legacy secret-material columns';
    END IF;

    -- Owner FK retained: user_id references users.id.
    IF NOT EXISTS (
        SELECT 1
        FROM information_schema.table_constraints tc
        JOIN information_schema.key_column_usage kcu
          USING (constraint_catalog, constraint_schema, constraint_name)
        JOIN information_schema.referential_constraints rc
          USING (constraint_catalog, constraint_schema, constraint_name)
        JOIN information_schema.key_column_usage ccu
          ON ccu.constraint_catalog = rc.unique_constraint_catalog
         AND ccu.constraint_schema = rc.unique_constraint_schema
         AND ccu.constraint_name = rc.unique_constraint_name
        WHERE tc.table_schema = 'public'
          AND tc.table_name = 'subsonic_credentials'
          AND tc.constraint_type = 'FOREIGN KEY'
          AND kcu.column_name = 'user_id'
          AND kcu.ordinal_position = ccu.ordinal_position
          AND ccu.table_schema = 'public'
          AND ccu.table_name = 'users'
          AND ccu.column_name = 'id'
    ) THEN
        RAISE EXCEPTION 'subsonic_credentials.user_id must FK to users.id';
    END IF;

    -- Unique enforcements, covering both UNIQUE constraints and bare UNIQUE
    -- indexes (constraints back onto indexes, so a set-based check sees both).
    CREATE TEMP TABLE subsonic_uniq_keys ON COMMIT DROP AS
    SELECT DISTINCT cols FROM (
        SELECT (SELECT array_agg(a.attname::text ORDER BY a.attname::text)
                FROM unnest(c.conkey) AS k(attnum)
                JOIN pg_attribute a
                  ON a.attrelid = c.conrelid AND a.attnum = k.attnum) AS cols
        FROM pg_constraint c
        WHERE c.conrelid = 'public.subsonic_credentials'::regclass
          AND c.contype = 'u'
        UNION ALL
        SELECT (SELECT array_agg(a.attname::text ORDER BY a.attname::text)
                FROM unnest(string_to_array(i.indkey::text, ' ')) AS k(s)
                JOIN pg_attribute a
                  ON a.attrelid = i.indrelid AND a.attnum = k.s::smallint) AS cols
        FROM pg_index i
        WHERE i.indrelid = 'public.subsonic_credentials'::regclass
          AND i.indisunique AND i.indisvalid AND i.indisready
          AND i.indexprs IS NULL AND i.indpred IS NULL
    ) s;
    IF NOT EXISTS (SELECT 1 FROM subsonic_uniq_keys
                   WHERE cols = ARRAY['api_key_hmac', 'pepper_version']) THEN
        RAISE EXCEPTION 'missing composite UNIQUE (pepper_version, api_key_hmac)';
    END IF;
    IF EXISTS (SELECT 1 FROM subsonic_uniq_keys
               WHERE cols = ARRAY['api_key_hmac']) THEN
        RAISE EXCEPTION 'api_key_hmac must not be globally unique';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM subsonic_uniq_keys
                   WHERE cols = ARRAY['subsonic_username']) THEN
        RAISE EXCEPTION 'subsonic_username must be unique';
    END IF;

    -- users.subsonic_disabled_at is a distinct nullable marker alongside the
    -- retained deleting_at (shape only; no disable semantics claimed here).
    PERFORM 1 FROM information_schema.columns
    WHERE table_schema = 'public' AND table_name = 'users'
      AND column_name = 'subsonic_disabled_at'
      AND data_type = 'timestamp with time zone' AND is_nullable = 'YES';
    IF NOT FOUND THEN
        RAISE EXCEPTION 'users.subsonic_disabled_at must be nullable TIMESTAMPTZ';
    END IF;
    PERFORM 1 FROM information_schema.columns
    WHERE table_schema = 'public' AND table_name = 'users'
      AND column_name = 'deleting_at';
    IF NOT FOUND THEN
        RAISE EXCEPTION 'users.deleting_at must be retained alongside subsonic_disabled_at';
    END IF;

    -- Synthetic fixtures: two owners. (users.created_at is the repo's epoch
    -- BIGINT; the credential table's own created_at is TIMESTAMPTZ.)
    INSERT INTO users (id, email, created_at)
    VALUES ('user-sub-1', 'sub-1@example.com', 1700000000),
           ('user-sub-2', 'sub-2@example.com', 1700000000);

    -- The disabled-at marker stores a timestamptz (type proof only).
    UPDATE users SET subsonic_disabled_at = TIMESTAMPTZ '2026-02-01T00:00:00Z'
    WHERE id = 'user-sub-2';
    IF (SELECT subsonic_disabled_at FROM users
        WHERE id = 'user-sub-2') IS NULL THEN
        RAISE EXCEPTION 'users.subsonic_disabled_at did not store a timestamptz';
    END IF;
    UPDATE users SET subsonic_disabled_at = NULL WHERE id = 'user-sub-2';

    -- Two clients for one owner (auth_mode/scope omitted on the first:
    -- defaults must fill apikey/subsonic) plus one client for a second owner.
    INSERT INTO subsonic_credentials
        (id, user_id, label, subsonic_username, api_key_hmac, pepper_version,
         created_at)
    VALUES ('11111111-1111-1111-1111-111111111111', 'user-sub-1', 'desktop',
            'sn-client-a1', hmac_a, 1, TIMESTAMPTZ '2026-01-01T00:00:00Z');
    INSERT INTO subsonic_credentials
        (id, user_id, label, subsonic_username, api_key_hmac, pepper_version,
         auth_mode, scope, created_at)
    VALUES ('22222222-2222-2222-2222-222222222222', 'user-sub-1', 'phone',
            'sn-client-a2', hmac_b, 1, 'apikey', 'subsonic',
            TIMESTAMPTZ '2026-01-01T00:00:00Z');
    INSERT INTO subsonic_credentials
        (id, user_id, label, subsonic_username, api_key_hmac, pepper_version,
         created_at)
    VALUES ('33333333-3333-3333-3333-333333333333', 'user-sub-2', 'desktop',
            'sn-client-b1', hmac_c, 1, TIMESTAMPTZ '2026-01-01T00:00:00Z');

    -- Defaults land on the row that omitted them.
    IF (SELECT auth_mode FROM subsonic_credentials
        WHERE subsonic_username = 'sn-client-a1') IS DISTINCT FROM 'apikey' THEN
        RAISE EXCEPTION 'auth_mode default did not fill apikey';
    END IF;
    IF (SELECT scope FROM subsonic_credentials
        WHERE subsonic_username = 'sn-client-a1') IS DISTINCT FROM 'subsonic' THEN
        RAISE EXCEPTION 'scope default did not fill subsonic';
    END IF;

    -- Composite identity: the same digest under a different pepper version
    -- is a different row (no global hmac uniqueness).
    INSERT INTO subsonic_credentials
        (id, user_id, label, subsonic_username, api_key_hmac, pepper_version,
         created_at)
    VALUES ('44444444-4444-4444-4444-444444444444', 'user-sub-1', 'rotated',
            'sn-client-a1r', hmac_a, 2, TIMESTAMPTZ '2026-01-01T00:00:00Z');

    -- Duplicate (pepper_version, api_key_hmac) cannot exist.
    BEGIN
        INSERT INTO subsonic_credentials
            (id, user_id, label, subsonic_username, api_key_hmac,
             pepper_version, created_at)
        VALUES ('55555555-5555-5555-5555-555555555555', 'user-sub-2', 'dup',
                'sn-client-dup', hmac_a, 1, TIMESTAMPTZ '2026-01-01T00:00:00Z');
        RAISE EXCEPTION 'UNIQUE accepted a duplicate (pepper_version, api_key_hmac)';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;

    -- Duplicate subsonic_username cannot exist.
    BEGIN
        INSERT INTO subsonic_credentials
            (id, user_id, label, subsonic_username, api_key_hmac,
             pepper_version, created_at)
        VALUES ('66666666-6666-6666-6666-666666666666', 'user-sub-2', 'dup',
                'sn-client-a1', hmac_d, 1, TIMESTAMPTZ '2026-01-01T00:00:00Z');
        RAISE EXCEPTION 'UNIQUE accepted a duplicate subsonic_username';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;

    -- NULL rejections on every NOT NULL column.
    BEGIN
        INSERT INTO subsonic_credentials
            (id, user_id, label, subsonic_username, api_key_hmac,
             pepper_version, created_at)
        VALUES ('66666666-6666-6666-6666-666666666666', 'user-sub-1', NULL,
                'sn-neg', hmac_d, 1, TIMESTAMPTZ '2026-01-01T00:00:00Z');
        RAISE EXCEPTION 'label accepted NULL';
    EXCEPTION WHEN not_null_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO subsonic_credentials
            (id, user_id, label, subsonic_username, api_key_hmac,
             pepper_version, created_at)
        VALUES ('66666666-6666-6666-6666-666666666666', 'user-sub-1', 'x',
                NULL, hmac_d, 1, TIMESTAMPTZ '2026-01-01T00:00:00Z');
        RAISE EXCEPTION 'subsonic_username accepted NULL';
    EXCEPTION WHEN not_null_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO subsonic_credentials
            (id, user_id, label, subsonic_username, api_key_hmac,
             pepper_version, created_at)
        VALUES ('66666666-6666-6666-6666-666666666666', 'user-sub-1', 'x',
                'sn-neg', NULL, 1, TIMESTAMPTZ '2026-01-01T00:00:00Z');
        RAISE EXCEPTION 'api_key_hmac accepted NULL';
    EXCEPTION WHEN not_null_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO subsonic_credentials
            (id, user_id, label, subsonic_username, api_key_hmac,
             pepper_version, created_at)
        VALUES ('66666666-6666-6666-6666-666666666666', 'user-sub-1', 'x',
                'sn-neg', hmac_d, NULL, TIMESTAMPTZ '2026-01-01T00:00:00Z');
        RAISE EXCEPTION 'pepper_version accepted NULL';
    EXCEPTION WHEN not_null_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO subsonic_credentials
            (id, user_id, label, subsonic_username, api_key_hmac,
             pepper_version, created_at)
        VALUES ('66666666-6666-6666-6666-666666666666', NULL, 'x',
                'sn-neg', hmac_d, 1, TIMESTAMPTZ '2026-01-01T00:00:00Z');
        RAISE EXCEPTION 'user_id accepted NULL';
    EXCEPTION WHEN not_null_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO subsonic_credentials
            (id, user_id, label, subsonic_username, api_key_hmac,
             pepper_version, created_at)
        VALUES (NULL, 'user-sub-1', 'x',
                'sn-neg', hmac_d, 1, TIMESTAMPTZ '2026-01-01T00:00:00Z');
        RAISE EXCEPTION 'id accepted NULL';
    EXCEPTION WHEN not_null_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO subsonic_credentials
            (id, user_id, label, subsonic_username, api_key_hmac,
             pepper_version, created_at)
        VALUES ('66666666-6666-6666-6666-666666666666', 'user-sub-1', 'x',
                'sn-neg', hmac_d, 1, NULL);
        RAISE EXCEPTION 'created_at accepted NULL';
    EXCEPTION WHEN not_null_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO subsonic_credentials
            (id, user_id, label, subsonic_username, api_key_hmac,
             pepper_version, auth_mode, created_at)
        VALUES ('66666666-6666-6666-6666-666666666666', 'user-sub-1', 'x',
                'sn-neg', hmac_d, 1, NULL, TIMESTAMPTZ '2026-01-01T00:00:00Z');
        RAISE EXCEPTION 'auth_mode accepted NULL';
    EXCEPTION WHEN not_null_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO subsonic_credentials
            (id, user_id, label, subsonic_username, api_key_hmac,
             pepper_version, scope, created_at)
        VALUES ('66666666-6666-6666-6666-666666666666', 'user-sub-1', 'x',
                'sn-neg', hmac_d, 1, NULL, TIMESTAMPTZ '2026-01-01T00:00:00Z');
        RAISE EXCEPTION 'scope accepted NULL';
    EXCEPTION WHEN not_null_violation THEN
        NULL;
    END;

    -- Digest shape: exactly 64 lowercase hex.
    BEGIN
        INSERT INTO subsonic_credentials
            (id, user_id, label, subsonic_username, api_key_hmac,
             pepper_version, created_at)
        VALUES ('66666666-6666-6666-6666-666666666666', 'user-sub-1', 'x',
                'sn-neg', repeat('a', 63), 1, TIMESTAMPTZ '2026-01-01T00:00:00Z');
        RAISE EXCEPTION 'api_key_hmac accepted a 63-char digest';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO subsonic_credentials
            (id, user_id, label, subsonic_username, api_key_hmac,
             pepper_version, created_at)
        VALUES ('66666666-6666-6666-6666-666666666666', 'user-sub-1', 'x',
                'sn-neg', repeat('a', 65), 1, TIMESTAMPTZ '2026-01-01T00:00:00Z');
        RAISE EXCEPTION 'api_key_hmac accepted a 65-char digest';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO subsonic_credentials
            (id, user_id, label, subsonic_username, api_key_hmac,
             pepper_version, created_at)
        VALUES ('66666666-6666-6666-6666-666666666666', 'user-sub-1', 'x',
                'sn-neg', repeat('A', 64), 1, TIMESTAMPTZ '2026-01-01T00:00:00Z');
        RAISE EXCEPTION 'api_key_hmac accepted uppercase hex';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO subsonic_credentials
            (id, user_id, label, subsonic_username, api_key_hmac,
             pepper_version, created_at)
        VALUES ('66666666-6666-6666-6666-666666666666', 'user-sub-1', 'x',
                'sn-neg', repeat('g', 64), 1, TIMESTAMPTZ '2026-01-01T00:00:00Z');
        RAISE EXCEPTION 'api_key_hmac accepted non-hex';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Pepper version is positive.
    BEGIN
        INSERT INTO subsonic_credentials
            (id, user_id, label, subsonic_username, api_key_hmac,
             pepper_version, created_at)
        VALUES ('66666666-6666-6666-6666-666666666666', 'user-sub-1', 'x',
                'sn-neg', hmac_d, 0, TIMESTAMPTZ '2026-01-01T00:00:00Z');
        RAISE EXCEPTION 'pepper_version accepted 0';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO subsonic_credentials
            (id, user_id, label, subsonic_username, api_key_hmac,
             pepper_version, created_at)
        VALUES ('66666666-6666-6666-6666-666666666666', 'user-sub-1', 'x',
                'sn-neg', hmac_d, -1, TIMESTAMPTZ '2026-01-01T00:00:00Z');
        RAISE EXCEPTION 'pepper_version accepted -1';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Restrict auth_mode/scope on updates too. Quote a synthetic literal so
    -- both TEXT+CHECK and ENUM implementations reject the value itself;
    -- passing a typed TEXT variable to an ENUM would instead test a cast error.
    FOR bad_auth_mode IN SELECT unnest(ARRAY['', 'APIKEY', 'bogus', 'token', 'both']) LOOP
        BEGIN
            EXECUTE format(
                'UPDATE subsonic_credentials SET auth_mode = %L WHERE subsonic_username = %L',
                bad_auth_mode, 'sn-client-a1');
            RAISE EXCEPTION 'auth_mode accepted %', bad_auth_mode;
        EXCEPTION WHEN check_violation OR invalid_text_representation THEN
            NULL;
        END;
    END LOOP;
    FOR bad_scope IN SELECT unnest(ARRAY['', 'SUBSONIC', 'bogus', 'native', 'ingest']) LOOP
        BEGIN
            EXECUTE format(
                'UPDATE subsonic_credentials SET scope = %L WHERE subsonic_username = %L',
                bad_scope, 'sn-client-a1');
            RAISE EXCEPTION 'scope accepted %', bad_scope;
        EXCEPTION WHEN check_violation OR invalid_text_representation THEN
            NULL;
        END;
    END LOOP;

    -- Owner fence: credentials cannot point at a missing user.
    BEGIN
        INSERT INTO subsonic_credentials
            (id, user_id, label, subsonic_username, api_key_hmac,
             pepper_version, created_at)
        VALUES ('66666666-6666-6666-6666-666666666666', 'user-sub-missing', 'x',
                'sn-neg', hmac_d, 1, TIMESTAMPTZ '2026-01-01T00:00:00Z');
        RAISE EXCEPTION 'FK accepted a credential for a missing user';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;
    END;

    -- Independent revocation: revoking one client of an owner leaves the
    -- owner's other clients live (column behavior only; no production
    -- revocation flow is asserted here).
    UPDATE subsonic_credentials
    SET revoked_at = TIMESTAMPTZ '2026-03-01T00:00:00Z'
    WHERE subsonic_username = 'sn-client-a1';
    UPDATE subsonic_credentials
    SET last_used_at = TIMESTAMPTZ '2026-02-15T00:00:00Z'
    WHERE subsonic_username = 'sn-client-a2';
    IF (SELECT revoked_at FROM subsonic_credentials
        WHERE subsonic_username = 'sn-client-a1') IS NULL THEN
        RAISE EXCEPTION 'revoked_at did not store on the revoked row';
    END IF;
    IF (SELECT revoked_at FROM subsonic_credentials
        WHERE subsonic_username = 'sn-client-a2') IS NOT NULL THEN
        RAISE EXCEPTION 'revoking one client revoked its sibling';
    END IF;
    IF (SELECT revoked_at FROM subsonic_credentials
        WHERE subsonic_username = 'sn-client-a1r') IS NOT NULL THEN
        RAISE EXCEPTION 'revoking one digest version revoked the other version';
    END IF;
    IF (SELECT count(*) FROM subsonic_credentials
        WHERE user_id = 'user-sub-1' AND revoked_at IS NULL) <> 2 THEN
        RAISE EXCEPTION 'expected exactly two live rows for user-sub-1 after revocation';
    END IF;
    IF (SELECT last_used_at FROM subsonic_credentials
        WHERE subsonic_username = 'sn-client-a2') IS NULL THEN
        RAISE EXCEPTION 'last_used_at did not store on the touched row';
    END IF;
    IF (SELECT last_used_at FROM subsonic_credentials
        WHERE subsonic_username = 'sn-client-a1') IS NOT NULL THEN
        RAISE EXCEPTION 'last_used_at leaked onto an untouched row';
    END IF;
END $$;

ROLLBACK;
