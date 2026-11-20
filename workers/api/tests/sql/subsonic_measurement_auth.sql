-- Runs under the disposable migration CI database. No login password or
-- fixture data survives this transaction.
BEGIN;
CREATE ROLE measurement_auth_test NOLOGIN NOINHERIT;
GRANT USAGE ON SCHEMA subsonic_measurement TO measurement_auth_test;
GRANT SELECT, UPDATE (id) ON subsonic_measurement.users TO measurement_auth_test;
GRANT SELECT, UPDATE (attempt_count, attempt_window, last_used_at, pepper_version, api_key_hmac)
    ON subsonic_measurement.subsonic_credentials TO measurement_auth_test;

INSERT INTO public.users (id, email, created_at) VALUES
    ('measure-owner', 'measure-owner@example.test', 1),
    ('measure-other', 'measure-other@example.test', 1);
INSERT INTO public.subsonic_credentials (id, user_id, subsonic_username, api_key_hmac, pepper_version)
VALUES
    ('00000000-0000-0000-0000-000000000001', 'measure-owner', 'measure-owner', repeat('1', 64), 1),
    ('00000000-0000-0000-0000-000000000002', 'measure-other', 'measure-other', repeat('2', 64), 1),
    ('00000000-0000-0000-0000-000000000003', 'measure-owner', 'measure-second', repeat('3', 64), 1);
SET LOCAL search_path = pg_catalog, subsonic_measurement;
SET SESSION AUTHORIZATION measurement_auth_test;
DO $$ BEGIN
    IF EXISTS (SELECT FROM users) OR EXISTS (SELECT FROM subsonic_credentials) THEN
        RAISE EXCEPTION 'unenrolled measurement views must be empty';
    END IF;
END $$;
RESET SESSION AUTHORIZATION;

INSERT INTO subsonic_measurement.fixture (user_id, credential_id)
VALUES ('measure-owner', '00000000-0000-0000-0000-000000000001');
SET SESSION AUTHORIZATION measurement_auth_test;
DO $$ DECLARE
    locked_id text;
    affected integer;
BEGIN
    SELECT id INTO STRICT locked_id FROM users WHERE deleting_at IS NULL FOR SHARE;
    IF locked_id <> 'measure-owner' OR
       (SELECT array_agg(c.subsonic_username) FROM subsonic_credentials c
        JOIN users u ON u.id = c.user_id) IS DISTINCT FROM ARRAY['measure-owner'] THEN
        RAISE EXCEPTION 'measurement did not see exactly the enrolled auth pair';
    END IF;

    UPDATE subsonic_credentials SET attempt_count = attempt_count + 1, attempt_window = 100
    WHERE revoked_at IS NULL;
    GET DIAGNOSTICS affected = ROW_COUNT;
    IF affected <> 1 THEN RAISE EXCEPTION 'admission must update one row'; END IF;
    UPDATE subsonic_credentials SET last_used_at = to_timestamp(6000),
        pepper_version = 2, api_key_hmac = repeat('4', 64)
    WHERE subsonic_username = 'measure-owner' AND attempt_count = 1;
    GET DIAGNOSTICS affected = ROW_COUNT;
    IF affected <> 1 THEN RAISE EXCEPTION 'completion must update one row'; END IF;

    BEGIN
        PERFORM id FROM public.users;
        RAISE EXCEPTION 'base user table readable';
    EXCEPTION WHEN insufficient_privilege THEN NULL;
    END;
    BEGIN
        PERFORM id FROM public.subsonic_credentials;
        RAISE EXCEPTION 'base credential table readable';
    EXCEPTION WHEN insufficient_privilege THEN NULL;
    END;
    BEGIN
        DELETE FROM fixture;
        RAISE EXCEPTION 'enrollment writable';
    EXCEPTION WHEN insufficient_privilege THEN NULL;
    END;
    BEGIN
        UPDATE users SET id = id;
        RAISE EXCEPTION 'owner id writable';
    EXCEPTION WHEN feature_not_supported THEN NULL;
    END;
    BEGIN
        UPDATE users SET deleting_at = now();
        RAISE EXCEPTION 'owner deletion state writable';
    EXCEPTION WHEN insufficient_privilege THEN NULL;
    END;
    BEGIN
        UPDATE subsonic_credentials SET revoked_at = now();
        RAISE EXCEPTION 'revocation writable';
    EXCEPTION WHEN insufficient_privilege THEN NULL;
    END;
END $$;
RESET SESSION AUTHORIZATION;
DO $$ BEGIN
    IF EXISTS (SELECT FROM public.subsonic_credentials
               WHERE subsonic_username IN ('measure-other', 'measure-second')
                 AND (attempt_count <> 0 OR pepper_version <> 1)) THEN
        RAISE EXCEPTION 'measurement changed an unenrolled row';
    END IF;
END $$;
UPDATE public.subsonic_credentials SET revoked_at = now() WHERE subsonic_username = 'measure-owner';
SET SESSION AUTHORIZATION measurement_auth_test;
DO $$ BEGIN
    IF EXISTS (SELECT FROM users) OR EXISTS (SELECT FROM subsonic_credentials) THEN
        RAISE EXCEPTION 'revoking the fixture must hide both auth rows';
    END IF;
END $$;
RESET SESSION AUTHORIZATION;
ROLLBACK;
