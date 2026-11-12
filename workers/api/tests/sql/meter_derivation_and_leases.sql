-- Regression test for migrations 0031 (meter derivation snapshots +
-- playback leases) and 0032 (ledger seconds floor). Run after ALL
-- migrations are applied (see ../run_migration_tests.sh). Everything
-- happens inside one transaction that is rolled back, so it leaves no
-- rows; any broken expectation RAISEs and (with ON_ERROR_STOP) fails
-- the run.
--
-- Scope: the 0020 nominal-rate CHECK is gone (a row whose seconds do
-- not equal ceil(bytes / 48000) is accepted), the decoupled range
-- CHECKs stay (with 0032's `>= 1` seconds floor), the nullable
-- `file_bytes` / `duration_ms` snapshot columns exist and reject
-- negatives, the sink upsert replaces the snapshot pair per advancing
-- flush (stored rows always recompute), and `playback_leases` exists
-- with its one-row-per-user primary key, cascading user FK, and bounds.

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
        RAISE EXCEPTION 'nominal-rate CHECK survived migration 0031';
    END IF;

    IF to_regclass('public.playback_leases') IS NULL THEN
        RAISE EXCEPTION 'playback_leases missing after 0031';
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

-- Pre-0031 shape still inserts (snapshots default NULL), and a
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

    -- Migration 0032: the seconds floor the retired rate CHECK used to
    -- imply. Zero seconds for positive bytes is rejected at the schema,
    -- exactly as prepare_ledger_row rejects it at the writer boundary.
    IF EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'public.stream_play_ledger'::regclass
          AND contype = 'c'
          AND pg_get_constraintdef(oid) LIKE '%stream_seconds >= 0%'
    ) THEN
        RAISE EXCEPTION 'pre-0032 zero-allowing seconds CHECK survived';
    END IF;
    BEGIN
        INSERT INTO stream_play_ledger
            (idempotency_key, user_id, track_id, period_month,
             stream_seconds, bytes_served, recorded_at)
        VALUES ('play:mdl-user:track-5:play-5', 'mdl-user', 'track-5',
                '2026-11', 0, 48000, 1761955200);
        RAISE EXCEPTION 'zero stream_seconds was accepted after 0032';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
END $$;

-- The sink upsert (ledger_play_window_upsert_statement, literals
-- inlined as in playback_lease_statements.sql) replaces the snapshot
-- pair on every advancing flush: stored seconds must always recompute
-- from stored inputs, so a fallback flush (NULL snapshots) must clear
-- the proportional snapshots an earlier flush left behind — a
-- per-column COALESCE would strand fallback seconds beside
-- proportional inputs.
INSERT INTO stream_play_ledger (idempotency_key, user_id, track_id, period_month, stream_seconds, bytes_served, recorded_at, file_bytes, duration_ms) SELECT 'play:mdl-user:track-6:play-6', 'mdl-user', 'track-6', '2026-11', 120, 15000000, 1761955200, 30000000, 240000 FROM (SELECT id FROM users WHERE id = 'mdl-user' AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (idempotency_key) DO UPDATE SET stream_seconds = EXCLUDED.stream_seconds, bytes_served = EXCLUDED.bytes_served, file_bytes = EXCLUDED.file_bytes, duration_ms = EXCLUDED.duration_ms WHERE EXCLUDED.bytes_served > stream_play_ledger.bytes_served;
-- A larger-union FALLBACK flush: ceil(16_000_000 / 48_000) = 334 s.
INSERT INTO stream_play_ledger (idempotency_key, user_id, track_id, period_month, stream_seconds, bytes_served, recorded_at, file_bytes, duration_ms) SELECT 'play:mdl-user:track-6:play-6', 'mdl-user', 'track-6', '2026-11', 334, 16000000, 1761955200, NULL, NULL FROM (SELECT id FROM users WHERE id = 'mdl-user' AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (idempotency_key) DO UPDATE SET stream_seconds = EXCLUDED.stream_seconds, bytes_served = EXCLUDED.bytes_served, file_bytes = EXCLUDED.file_bytes, duration_ms = EXCLUDED.duration_ms WHERE EXCLUDED.bytes_served > stream_play_ledger.bytes_served;
-- An older (smaller-union) redelivery never rewinds the row.
INSERT INTO stream_play_ledger (idempotency_key, user_id, track_id, period_month, stream_seconds, bytes_served, recorded_at, file_bytes, duration_ms) SELECT 'play:mdl-user:track-6:play-6', 'mdl-user', 'track-6', '2026-11', 124, 15500000, 1761955200, 30000000, 240000 FROM (SELECT id FROM users WHERE id = 'mdl-user' AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (idempotency_key) DO UPDATE SET stream_seconds = EXCLUDED.stream_seconds, bytes_served = EXCLUDED.bytes_served, file_bytes = EXCLUDED.file_bytes, duration_ms = EXCLUDED.duration_ms WHERE EXCLUDED.bytes_served > stream_play_ledger.bytes_served;

DO $$
DECLARE
    row_seconds BIGINT;
    row_bytes BIGINT;
    row_file BIGINT;
    row_duration BIGINT;
BEGIN
    SELECT stream_seconds, bytes_served, file_bytes, duration_ms
      INTO row_seconds, row_bytes, row_file, row_duration
      FROM stream_play_ledger
     WHERE idempotency_key = 'play:mdl-user:track-6:play-6';
    -- Fallback flush won and cleared the snapshot pair; the older
    -- redelivery changed nothing. The row recomputes as a fallback row.
    IF row_seconds <> 334 OR row_bytes <> 16000000 THEN
        RAISE EXCEPTION 'upsert kept % s / % bytes, expected 334 / 16000000',
            row_seconds, row_bytes;
    END IF;
    IF row_file IS NOT NULL OR row_duration IS NOT NULL THEN
        RAISE EXCEPTION 'fallback flush stranded proportional snapshots (% / %)',
            row_file, row_duration;
    END IF;
    IF row_seconds <> ceil(row_bytes::numeric / 48000) THEN
        RAISE EXCEPTION 'fallback row does not recompute from bytes alone';
    END IF;
END $$;

-- A later proportional flush (the full file) restores the pair, and the
-- row recomputes as ceil(union * duration / file) once more.
INSERT INTO stream_play_ledger (idempotency_key, user_id, track_id, period_month, stream_seconds, bytes_served, recorded_at, file_bytes, duration_ms) SELECT 'play:mdl-user:track-6:play-6', 'mdl-user', 'track-6', '2026-11', 240, 30000000, 1761955200, 30000000, 240000 FROM (SELECT id FROM users WHERE id = 'mdl-user' AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (idempotency_key) DO UPDATE SET stream_seconds = EXCLUDED.stream_seconds, bytes_served = EXCLUDED.bytes_served, file_bytes = EXCLUDED.file_bytes, duration_ms = EXCLUDED.duration_ms WHERE EXCLUDED.bytes_served > stream_play_ledger.bytes_served;

DO $$
DECLARE
    row_seconds BIGINT;
    row_bytes BIGINT;
    row_file BIGINT;
    row_duration BIGINT;
BEGIN
    SELECT stream_seconds, bytes_served, file_bytes, duration_ms
      INTO row_seconds, row_bytes, row_file, row_duration
      FROM stream_play_ledger
     WHERE idempotency_key = 'play:mdl-user:track-6:play-6';
    IF row_seconds <> 240 OR row_file <> 30000000 OR row_duration <> 240000 THEN
        RAISE EXCEPTION 'proportional flush did not restore the snapshot pair';
    END IF;
    IF row_seconds <> ceil(row_bytes::numeric * row_duration / (row_file * 1000)) THEN
        RAISE EXCEPTION 'proportional row does not recompute from its snapshots';
    END IF;
END $$;

-- One lease row per user; a second insert for the same user conflicts
-- (the api Worker upserts on this key).
INSERT INTO playback_leases
    (user_id, session_id, track_id, play_id, epoch, expires_at, updated_at)
VALUES ('mdl-user', 'sess-1', 'track-1', 'play-1', 1, 1761955245, 1761955200);

DO $$
BEGIN
    -- Migration 0041 preserves the legacy insert shape with a zero deadline.
    IF NOT EXISTS (SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'playback_leases'
          AND column_name = 'continuation_until' AND data_type = 'bigint'
          AND is_nullable = 'NO' AND column_default IS NOT NULL) THEN
        RAISE EXCEPTION 'continuation deadline column has wrong shape';
    END IF;
    IF (SELECT continuation_until FROM playback_leases WHERE user_id = 'mdl-user') IS DISTINCT FROM 0 THEN
        RAISE EXCEPTION 'legacy lease insert must default continuation deadline to zero';
    END IF;
    BEGIN
        UPDATE playback_leases SET continuation_until = -1 WHERE user_id = 'mdl-user';
        RAISE EXCEPTION 'negative continuation deadline was accepted';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

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
