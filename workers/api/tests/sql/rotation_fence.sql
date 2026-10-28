-- Isolated scratch-Postgres regression for the invocation ownership fence.
-- The lease uses the database clock; no token is a key or credential.
-- Every statement rolls back, including the singleton's test ownership.
BEGIN;
DO $$
DECLARE
    changed BIGINT;
    accepted BOOLEAN;
BEGIN
    INSERT INTO rotation_state (id, token, expires_at)
    VALUES ('vault-rewrap', 'fence-test-first', FLOOR(EXTRACT(EPOCH FROM clock_timestamp()))::BIGINT + 120);

    -- A second live invocation, even with another ceremony label/target,
    -- cannot acquire the database-wide singleton before expiry.
    INSERT INTO rotation_state (id, token, expires_at)
    VALUES ('vault-rewrap', 'fence-test-second', FLOOR(EXTRACT(EPOCH FROM clock_timestamp()))::BIGINT + 120)
    ON CONFLICT (id) DO UPDATE SET token = EXCLUDED.token, expires_at = EXCLUDED.expires_at
    WHERE rotation_state.expires_at <= FLOOR(EXTRACT(EPOCH FROM clock_timestamp()))::BIGINT;
    GET DIAGNOSTICS changed = ROW_COUNT;
    IF changed <> 0 THEN RAISE EXCEPTION 'live ownership was replaced'; END IF;

    SELECT EXISTS (SELECT id FROM rotation_state WHERE id = 'vault-rewrap' AND token = 'fence-test-first'
        AND expires_at > FLOOR(EXTRACT(EPOCH FROM clock_timestamp()))::BIGINT FOR UPDATE) INTO accepted;
    IF NOT accepted THEN RAISE EXCEPTION 'current ownership refused'; END IF;

    UPDATE rotation_state SET expires_at = 0 WHERE id = 'vault-rewrap';
    SELECT EXISTS (SELECT id FROM rotation_state WHERE id = 'vault-rewrap' AND token = 'fence-test-first'
        AND expires_at > FLOOR(EXTRACT(EPOCH FROM clock_timestamp()))::BIGINT FOR UPDATE) INTO accepted;
    IF accepted THEN RAISE EXCEPTION 'expired ownership accepted'; END IF;

    INSERT INTO rotation_state (id, token, expires_at)
    VALUES ('vault-rewrap', 'fence-test-second', FLOOR(EXTRACT(EPOCH FROM clock_timestamp()))::BIGINT + 120)
    ON CONFLICT (id) DO UPDATE SET token = EXCLUDED.token, expires_at = EXCLUDED.expires_at
    WHERE rotation_state.expires_at <= FLOOR(EXTRACT(EPOCH FROM clock_timestamp()))::BIGINT;
    GET DIAGNOSTICS changed = ROW_COUNT;
    IF changed <> 1 THEN RAISE EXCEPTION 'expiry did not allow takeover'; END IF;

    SELECT EXISTS (SELECT id FROM rotation_state WHERE id = 'vault-rewrap' AND token = 'fence-test-first'
        AND expires_at > FLOOR(EXTRACT(EPOCH FROM clock_timestamp()))::BIGINT FOR UPDATE) INTO accepted;
    IF accepted THEN RAISE EXCEPTION 'stale owner passed after takeover'; END IF;

    UPDATE rotation_state SET expires_at = 0 WHERE id = 'vault-rewrap' AND token = 'fence-test-first';
    GET DIAGNOSTICS changed = ROW_COUNT;
    IF changed <> 0 THEN RAISE EXCEPTION 'stale release changed new ownership'; END IF;
    SELECT EXISTS (SELECT id FROM rotation_state WHERE id = 'vault-rewrap' AND token = 'fence-test-second'
        AND expires_at > FLOOR(EXTRACT(EPOCH FROM clock_timestamp()))::BIGINT FOR UPDATE) INTO accepted;
    IF NOT accepted THEN RAISE EXCEPTION 'new ownership lost'; END IF;
END $$;
ROLLBACK;
