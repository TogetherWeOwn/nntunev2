-- Real-Postgres regression for the production `rotation_runs` ledger
-- and the positional claim/CAS/ledger statement shapes the Hyperdrive
-- `RewrapStore` executes. Run after ALL migrations are applied (see
-- ../run_migration_tests.sh). Everything happens inside one
-- transaction that is rolled back, so it leaves no rows; any broken
-- expectation RAISEs and (with ON_ERROR_STOP) fails the run.
--
-- This is the Postgres proof for the scheduled re-wrap slice. SQLite
-- and source-shape checks elsewhere deliberately do NOT stand in for
-- it: the dynamic `EXECUTE ... USING` statements below bind through
-- the Postgres extended-query path, so a statement Postgres cannot
-- actually plan (for example, the runbook's NAMED `$new`/`$w`
-- placeholders sent untranslated, or a `LIMIT` bound as text) fails
-- here instead of in the first live ceremony. Bind order for the CAS
-- mirrors the normative statement rendered positionally:
-- $1 new version, $2 new wrap, $3 new wrap nonce, $4 pass time (both
-- timestamp columns), $5 user, $6 captured version, $7 captured wrap,
-- $8 captured wrap nonce. All vault material below is synthetic text.

BEGIN;

DO $$
DECLARE
    affected INTEGER;
    claimed INTEGER;
    first_claim TEXT;
    rec RECORD;
    cols TEXT;
    idxdef TEXT;
BEGIN
    -- Schema: the reconciled record, columns in order, plus the
    -- partial-unique open-run guard.
    SELECT string_agg(column_name, ',' ORDER BY ordinal_position) INTO cols
    FROM information_schema.columns
    WHERE table_name = 'rotation_runs';
    IF cols IS DISTINCT FROM
       'id,env,from_version,to_version,cursor,rows_done,rows_failed,rows_deferred,claims_retried,started_at,updated_at,finished_at' THEN
        RAISE EXCEPTION 'rotation_runs columns drifted from the durable run record: %', cols;
    END IF;
    SELECT indexdef INTO idxdef FROM pg_indexes
    WHERE indexname = 'idx_rotation_runs_open';
    IF idxdef IS NULL OR idxdef NOT LIKE '%finished_at IS NULL%' THEN
        RAISE EXCEPTION 'idx_rotation_runs_open must be the partial open-run index: %', idxdef;
    END IF;

    INSERT INTO users (id, email, created_at)
    VALUES ('user-rr-1', 'rr-1@example.com', 1700000000),
           ('user-rr-2', 'rr-2@example.com', 1700000000);

    INSERT INTO provider_vaults
        (user_id, kek_version, dek_wrapped_nonce, dek_wrapped,
         payload_nonce, payload, updated_at)
    VALUES ('user-rr-1', 1, 'nonce-rr-1-old', 'wrap-rr-1-old',
            'pnonce-rr-1', 'payload-rr-1', 1700000000),
           ('user-rr-2', 1, 'nonce-rr-2-old', 'wrap-rr-2-old',
            'pnonce-rr-2', 'payload-rr-2', 1700000000);

    -- Normative claim, positionally bound: the batch LIMIT is a real
    -- bound parameter (1 row, first in user_id order), never OFFSET.
    claimed := 0;
    first_claim := NULL;
    FOR rec IN EXECUTE
        'SELECT user_id, kek_version, dek_wrapped, dek_wrapped_nonce
           FROM provider_vaults
          WHERE kek_version < $1 AND user_id > $2
          ORDER BY user_id LIMIT $3'
        USING 2, '', 1
    LOOP
        claimed := claimed + 1;
        IF first_claim IS NULL THEN
            first_claim := rec.user_id;
        END IF;
    END LOOP;
    IF claimed <> 1 OR first_claim IS DISTINCT FROM 'user-rr-1' THEN
        RAISE EXCEPTION 'bound claim must return one row in cursor order (got %, first %)',
            claimed, first_claim;
    END IF;

    -- A same-version vault PUT replaces the whole envelope underneath
    -- the captured claim.
    UPDATE provider_vaults
    SET dek_wrapped = 'wrap-rr-1-put', dek_wrapped_nonce = 'nonce-rr-1-put',
        payload = 'payload-rr-1-put', payload_nonce = 'pnonce-rr-1-put',
        updated_at = 1700000100
    WHERE user_id = 'user-rr-1';

    -- Stale captured-envelope CAS: zero rows, replacement survives.
    EXECUTE
        'UPDATE provider_vaults
            SET kek_version = $1, dek_wrapped = $2, dek_wrapped_nonce = $3,
                updated_at = $4, rewrapped_at = $4
          WHERE user_id = $5 AND kek_version = $6
            AND dek_wrapped = $7 AND dek_wrapped_nonce = $8'
        USING 2, 'wrap-rr-1-stale', 'nonce-rr-1-stale', 1800000000,
              'user-rr-1', 1, 'wrap-rr-1-old', 'nonce-rr-1-old';
    GET DIAGNOSTICS affected = ROW_COUNT;
    IF affected <> 0 THEN
        RAISE EXCEPTION 'stale captured-envelope CAS must lose with zero rows (got %)', affected;
    END IF;
    IF (SELECT payload FROM provider_vaults WHERE user_id = 'user-rr-1')
        IS DISTINCT FROM 'payload-rr-1-put' THEN
        RAISE EXCEPTION 'stale CAS overwrote the replacement payload';
    END IF;

    -- Reload-and-retry CAS against the current envelope: one row,
    -- wrap-only change, both timestamps from the single $4 bind.
    EXECUTE
        'UPDATE provider_vaults
            SET kek_version = $1, dek_wrapped = $2, dek_wrapped_nonce = $3,
                updated_at = $4, rewrapped_at = $4
          WHERE user_id = $5 AND kek_version = $6
            AND dek_wrapped = $7 AND dek_wrapped_nonce = $8'
        USING 2, 'wrap-rr-1-new', 'nonce-rr-1-new', 1800000000,
              'user-rr-1', 1, 'wrap-rr-1-put', 'nonce-rr-1-put';
    GET DIAGNOSTICS affected = ROW_COUNT;
    IF affected <> 1 THEN
        RAISE EXCEPTION 'current-envelope CAS must update exactly one row (got %)', affected;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM provider_vaults
        WHERE user_id = 'user-rr-1' AND kek_version = 2
          AND dek_wrapped = 'wrap-rr-1-new'
          AND dek_wrapped_nonce = 'nonce-rr-1-new'
          AND payload = 'payload-rr-1-put'
          AND payload_nonce = 'pnonce-rr-1-put'
          AND updated_at = 1800000000 AND rewrapped_at = 1800000000
    ) THEN
        RAISE EXCEPTION 'CAS must change wrap columns only and preserve payload bytes/nonce';
    END IF;

    -- Open a run through the store's bind shape; the partial-unique
    -- index rejects a second open run for the same (env, to_version).
    EXECUTE
        'INSERT INTO rotation_runs
            (id, env, from_version, to_version, cursor, rows_done, rows_failed,
             rows_deferred, claims_retried, started_at, updated_at, finished_at)
         VALUES ($1, $2, $3, $4, NULL, 0, 0, 0, 0, $5, $5, NULL)'
        USING 'run-rr-1', 'pg-acceptance-env', 1, 2, 1700000000;
    BEGIN
        EXECUTE
            'INSERT INTO rotation_runs
                (id, env, from_version, to_version, cursor, rows_done, rows_failed,
                 rows_deferred, claims_retried, started_at, updated_at, finished_at)
             VALUES ($1, $2, $3, $4, NULL, 0, 0, 0, 0, $5, $5, NULL)'
            USING 'run-rr-2', 'pg-acceptance-env', 1, 2, 1700000000;
        RAISE EXCEPTION 'partial-unique index accepted a second open run';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;

    -- Counters default to zero and the resume fields start empty when
    -- an insert relies on the column defaults.
    INSERT INTO rotation_runs (id, env, from_version, to_version, started_at, updated_at)
    VALUES ('run-rr-other-env', 'other-env', 1, 2, 1700000000, 1700000000);
    IF NOT EXISTS (
        SELECT 1 FROM rotation_runs
        WHERE id = 'run-rr-other-env' AND cursor IS NULL AND finished_at IS NULL
          AND rows_done = 0 AND rows_failed = 0
          AND rows_deferred = 0 AND claims_retried = 0
    ) THEN
        RAISE EXCEPTION 'ledger counters/cursor must default to a fresh open run';
    END IF;

    -- Progress save lands on the unfinished run; the resume load sees
    -- cursor and every counter durably.
    EXECUTE
        'UPDATE rotation_runs
            SET cursor = $1, rows_done = $2, rows_failed = $3,
                rows_deferred = $4, claims_retried = $5, updated_at = $6
          WHERE id = $7 AND finished_at IS NULL'
        USING 'user-rr-1', 7, 1, 2, 3, 1700000100, 'run-rr-1';
    GET DIAGNOSTICS affected = ROW_COUNT;
    IF affected <> 1 THEN
        RAISE EXCEPTION 'progress save must update the open run (got %)', affected;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM rotation_runs
        WHERE env = 'pg-acceptance-env' AND to_version = 2 AND finished_at IS NULL
          AND id = 'run-rr-1' AND cursor = 'user-rr-1'
          AND rows_done = 7 AND rows_failed = 1
          AND rows_deferred = 2 AND claims_retried = 3
          AND updated_at = 1700000100
    ) THEN
        RAISE EXCEPTION 'resume load must return the persisted cursor and counters';
    END IF;

    -- Close stamps once; a repeat close and any late progress save are
    -- zero-row no-ops the store can distinguish.
    EXECUTE
        'UPDATE rotation_runs SET finished_at = $1 WHERE id = $2 AND finished_at IS NULL'
        USING 1700000200, 'run-rr-1';
    GET DIAGNOSTICS affected = ROW_COUNT;
    IF affected <> 1 THEN
        RAISE EXCEPTION 'close must stamp the open run once (got %)', affected;
    END IF;
    EXECUTE
        'UPDATE rotation_runs SET finished_at = $1 WHERE id = $2 AND finished_at IS NULL'
        USING 1700000300, 'run-rr-1';
    GET DIAGNOSTICS affected = ROW_COUNT;
    IF affected <> 0 THEN
        RAISE EXCEPTION 'double close must be a zero-row no-op (got %)', affected;
    END IF;
    EXECUTE
        'UPDATE rotation_runs
            SET cursor = $1, rows_done = $2, rows_failed = $3,
                rows_deferred = $4, claims_retried = $5, updated_at = $6
          WHERE id = $7 AND finished_at IS NULL'
        USING 'user-rr-2', 99, 0, 0, 0, 1700000400, 'run-rr-1';
    GET DIAGNOSTICS affected = ROW_COUNT;
    IF affected <> 0 THEN
        RAISE EXCEPTION 'progress must never land on a closed run (got %)', affected;
    END IF;
    IF (SELECT rows_done FROM rotation_runs WHERE id = 'run-rr-1') <> 7 THEN
        RAISE EXCEPTION 'closed run counters changed after close';
    END IF;

    -- Closing frees the (env, to_version) slot for the next ceremony.
    EXECUTE
        'INSERT INTO rotation_runs
            (id, env, from_version, to_version, cursor, rows_done, rows_failed,
             rows_deferred, claims_retried, started_at, updated_at, finished_at)
         VALUES ($1, $2, $3, $4, NULL, 0, 0, 0, 0, $5, $5, NULL)'
        USING 'run-rr-next', 'pg-acceptance-env', 1, 2, 1700000500;
END $$;

ROLLBACK;
