-- Regression test for migration 0006. Run after
-- ALL migrations are applied (see ../run_migration_tests.sh). Everything
-- happens inside one transaction that is rolled back, so it leaves no rows;
-- any broken expectation RAISEs and (with ON_ERROR_STOP) fails the run.

BEGIN;

DO $$
BEGIN
    -- Handler purposes and the empty default are storable.
    INSERT INTO auth_challenges (id, rp_id, origin, expires_at, purpose)
    VALUES ('c-register', 'example.com', 'https://example.com', 9999999999, 'register'),
           ('c-login',    'example.com', 'https://example.com', 9999999999, 'login'),
           ('c-legacy',   'example.com', 'https://example.com', 9999999999, '');
    INSERT INTO auth_challenges (id, rp_id, origin, expires_at)
    VALUES ('c-default', 'example.com', 'https://example.com', 9999999999);
    IF (SELECT purpose FROM auth_challenges WHERE id = 'c-default') <> '' THEN
        RAISE EXCEPTION 'default purpose must stay the empty (fail-closed) value';
    END IF;

    -- CHECK: unknown purposes cannot be inserted.
    BEGIN
        INSERT INTO auth_challenges (id, rp_id, origin, expires_at, purpose)
        VALUES ('c-bogus', 'example.com', 'https://example.com', 9999999999, 'bogus');
        RAISE EXCEPTION 'CHECK accepted an unknown purpose';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- CHECK: case variants are different values.
    BEGIN
        INSERT INTO auth_challenges (id, rp_id, origin, expires_at, purpose)
        VALUES ('c-upper', 'example.com', 'https://example.com', 9999999999, 'LOGIN');
        RAISE EXCEPTION 'CHECK accepted a case variant of a purpose';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Trigger: a live challenge cannot be re-purposed in either direction.
    BEGIN
        UPDATE auth_challenges SET purpose = 'register' WHERE id = 'c-login';
        RAISE EXCEPTION 'login challenge was re-purposed to register';
    EXCEPTION WHEN restrict_violation THEN
        NULL;
    END;
    BEGIN
        UPDATE auth_challenges SET purpose = 'login' WHERE id = 'c-register';
        RAISE EXCEPTION 'register challenge was re-purposed to login';
    EXCEPTION WHEN restrict_violation THEN
        NULL;
    END;

    -- Trigger: a legacy ('') challenge cannot be upgraded to a usable one.
    BEGIN
        UPDATE auth_challenges SET purpose = 'register' WHERE id = 'c-legacy';
        RAISE EXCEPTION 'legacy empty-purpose challenge was upgraded';
    EXCEPTION WHEN restrict_violation THEN
        NULL;
    END;

    -- Trigger applies to every row of a multi-row UPDATE, atomically.
    BEGIN
        UPDATE auth_challenges SET purpose = 'login';
        RAISE EXCEPTION 'bulk re-purpose succeeded';
    EXCEPTION WHEN restrict_violation THEN
        NULL;
    END;

    -- The purposes are unchanged after all the rejected attempts.
    IF (SELECT array_agg(purpose ORDER BY id) FROM auth_challenges)
       IS DISTINCT FROM ARRAY['', '', 'login', 'register']::text[] THEN
        RAISE EXCEPTION 'purposes changed by rejected updates';
    END IF;

    -- Updates that do not change purpose keep working: the single-use
    -- claim and a no-op rewrite of the same value.
    UPDATE auth_challenges SET consumed = TRUE WHERE id = 'c-login' AND consumed = FALSE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'single-use claim (consumed = TRUE) was blocked';
    END IF;
    UPDATE auth_challenges SET purpose = 'login' WHERE id = 'c-login';
END
$$;

ROLLBACK;
