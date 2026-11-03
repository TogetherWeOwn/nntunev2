-- Regression test for migration 0029 (meter derivation snapshots +
-- playback leases). Run after ALL migrations are applied (see
-- ../run_migration_tests.sh). Everything happens inside one transaction
-- that is rolled back, so it leaves no rows; any broken expectation
-- RAISEs and (with ON_ERROR_STOP) fails the run.
--
-- Scope: the 0020 nominal-rate CHECK is gone (a row whose seconds do
-- not equal ceil(bytes / 48000) is accepted), the decoupled range
-- CHECKs stay, the nullable `file_bytes` / `duration_ms` snapshot
-- columns exist and reject negatives, and `playback_leases` exists with
-- its one-row-per-user primary key, cascading user FK, and bounds.

BEGIN;

INSERT INTO users (id, email, created_at)
VALUES ('mdl-user', 'mdl-user@example.com', 1000);

DO $$
BEGIN
    -- Snapshot columns exist, BIGINT, nullable.
    IF (SELECT count(*) FROM information_schema.columns
        WHERE table_name = 'stream_play_ledger'
          AND column_name IN ('file_bytes', 'duration_ms')
          AND data_type = 'bigint'
          AND is_nullable = 'YES') <> 2 THEN
        RAISE EXCEPTION 'stream_play_ledger snapshot columns missing or wrong shape';
    END IF;

    -- No CHECK on the ledger may still encode the nominal rate.
    IF EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'public.stream_play_ledger'::regclass
          AND contype = 'c'
          AND pg_get_constraintdef(oid) LIKE '%48000%'
    ) THEN
        RAISE EXCEPTION 'nominal-rate CHECK survived migration 0029';
    END IF;

    IF to_regclass('public.playback_leases') IS NULL THEN
        RAISE EXCEPTION 'playback_leases missing after 0029';
    END IF;
END $$;

-- The proportional derivation's canonical row: half of a 30 MB /
-- 240 s file bills 120 s. Under the retired rate the same bytes would
-- have required ceil(15_000_000 / 48_000) = 313 s, so this insert
-- proves the rate CHECK is really gone. Snapshots ride along.
INSERT INTO stream_play_ledger
    (idempotency_key, user_id, track_id, period_month, stream_seconds,
     bytes_served, recorded_at, file_bytes, duration_ms)
VALUES ('play:mdl-user:track-1:play-1', 'mdl-user', 'track-1', '2026-11',
        120, 15000000, 1761955200, 30000000, 240000);

-- Pre-0029 shape still inserts (snapshots default NULL), and a
-- fallback-rate row keeps NULL snapshots.
INSERT INTO stream_play_ledger
    (idempotency_key, user_id, track_id, period_month, stream_seconds,
     bytes_served, recorded_at)
VALUES ('play:mdl-user:track-2:play-2', 'mdl-user', 'track-2', '2026-11',
        1, 48000, 1761955200);

DO $$
BEGIN
    IF (SELECT file_bytes FROM stream_play_ledger
        WHERE idempotency_key = 'play:mdl-user:track-2:play-2') IS NOT NULL THEN
        RAISE EXCEPTION 'legacy-shape row must keep NULL snapshots';
    END IF;

    -- Decoupled range caps still bite: seconds above 86_400 rejected.
    BEGIN
        INSERT INTO stream_play_ledger
            (idempotency_key, user_id, track_id, period_month,
             stream_seconds, bytes_served, recorded_at)
        VALUES ('play:mdl-user:track-3:play-3', 'mdl-user', 'track-3',
                '2026-11', 86401, 48000, 1761955200);
        RAISE EXCEPTION 'stream_seconds above the row cap was accepted';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Negative snapshots rejected.
    BEGIN
        INSERT INTO stream_play_ledger
            (idempotency_key, user_id, track_id, period_month,
             stream_seconds, bytes_served, recorded_at, file_bytes)
        VALUES ('play:mdl-user:track-4:play-4', 'mdl-user', 'track-4',
                '2026-11', 1, 48000, 1761955200, -1);
        RAISE EXCEPTION 'negative file_bytes snapshot was accepted';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
END $$;

-- One lease row per user; a second insert for the same user conflicts
-- (the api Worker upserts on this key).
INSERT INTO playback_leases
    (user_id, session_id, track_id, play_id, epoch, expires_at, updated_at)
VALUES ('mdl-user', 'sess-1', 'track-1', 'play-1', 1, 1761955245, 1761955200);

DO $$
BEGIN
    BEGIN
        INSERT INTO playback_leases
            (user_id, session_id, track_id, play_id, epoch, expires_at,
             updated_at)
        VALUES ('mdl-user', 'sess-2', 'track-9', 'play-9', 1,
                1761955245, 1761955200);
        RAISE EXCEPTION 'second lease row for one user was accepted';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;

    -- Epoch starts at 1.
    BEGIN
        INSERT INTO playback_leases
            (user_id, session_id, track_id, play_id, epoch, expires_at,
             updated_at)
        VALUES ('mdl-user', 'sess-1', 'track-1', 'play-1', 0,
                1761955245, 1761955200)
        ON CONFLICT (user_id) DO UPDATE SET epoch = EXCLUDED.epoch;
        RAISE EXCEPTION 'epoch 0 was accepted';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Lease for a nonexistent user is refused by the FK.
    BEGIN
        INSERT INTO playback_leases
            (user_id, session_id, track_id, play_id, epoch, expires_at,
             updated_at)
        VALUES ('mdl-ghost', 'sess-1', 'track-1', 'play-1', 1,
                1761955245, 1761955200);
        RAISE EXCEPTION 'lease for a nonexistent user was accepted';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;
    END;
END $$;

-- Deleting the user cascades both tables (erasure leaves nothing).
DELETE FROM users WHERE id = 'mdl-user';

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM playback_leases WHERE user_id = 'mdl-user') THEN
        RAISE EXCEPTION 'playback lease survived user deletion';
    END IF;
    IF EXISTS (SELECT 1 FROM stream_play_ledger WHERE user_id = 'mdl-user') THEN
        RAISE EXCEPTION 'ledger rows survived user deletion';
    END IF;
END $$;

ROLLBACK;
