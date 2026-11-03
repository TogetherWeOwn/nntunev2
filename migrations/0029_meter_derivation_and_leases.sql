-- NNTune v2 migration 0029: meter derivation snapshots + playback leases.
--
-- Two changes for the playback-enforcement / hour-meter slice:
--
-- 1. `stream_play_ledger`: migration 0020 enforced the nominal-rate
--    derivation in the schema (`stream_seconds = ceil(bytes_served /
--    48000)`) and sized the byte cap off the same constant. That rate
--    over-counts FLAC roughly 2-3.7x, so the derivation moves to
--    delivered-bytes proportion of the track
--    (`ceil(union_bytes * duration / file_size)`, clamped) computed by
--    the api Worker at sink time. The rate CHECK is dropped; the range
--    CHECKs stay as decoupled sanity caps. Nullable `file_bytes` /
--    `duration_ms` snapshot columns record the inputs each row's
--    seconds were derived from, so any row can be recomputed in a
--    dispute. Rows written before this migration keep NULL snapshots.
--
-- 2. `playback_leases`: the one-concurrent-stream lease, one row per
--    user, conditionally upserted by the api Worker at grant issuance.
--    `epoch` increments on every acquire/takeover and rides in the
--    signed play grant; `expires_at` is the lease deadline (TTL 45 s,
--    heartbeat-extended). Timestamps are BIGINT Unix seconds, matching
--    the ledger's `recorded_at` domain.
--
-- Replay-safety (run_migration_tests.sh re-applies every migration):
-- the CHECK drop is guarded on the constraint still existing (found by
-- its definition, never by a guessed auto-generated name), the column
-- adds use IF NOT EXISTS, and the lease table is CREATE IF NOT EXISTS.

DO $$
DECLARE
    rate_check_name TEXT;
BEGIN
    SELECT conname INTO rate_check_name
    FROM pg_constraint
    WHERE conrelid = 'public.stream_play_ledger'::regclass
      AND contype = 'c'
      AND pg_get_constraintdef(oid) LIKE '%48000%';
    IF rate_check_name IS NOT NULL THEN
        EXECUTE format(
            'ALTER TABLE public.stream_play_ledger DROP CONSTRAINT %I',
            rate_check_name
        );
    END IF;
END $$;

ALTER TABLE stream_play_ledger
    ADD COLUMN IF NOT EXISTS file_bytes BIGINT NULL
        CHECK (file_bytes IS NULL OR file_bytes >= 0);
ALTER TABLE stream_play_ledger
    ADD COLUMN IF NOT EXISTS duration_ms BIGINT NULL
        CHECK (duration_ms IS NULL OR duration_ms >= 0);

CREATE TABLE IF NOT EXISTS playback_leases (
    user_id    TEXT PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE
               CHECK (char_length(user_id) BETWEEN 1 AND 128),
    session_id TEXT NOT NULL CHECK (char_length(session_id) BETWEEN 1 AND 128),
    track_id   TEXT NOT NULL CHECK (char_length(track_id) BETWEEN 1 AND 512),
    play_id    TEXT NOT NULL CHECK (char_length(play_id) BETWEEN 1 AND 512),
    epoch      BIGINT NOT NULL CHECK (epoch >= 1),
    expires_at BIGINT NOT NULL CHECK (expires_at >= 0 AND expires_at <= 253402300799),
    updated_at BIGINT NOT NULL CHECK (updated_at >= 0 AND updated_at <= 253402300799)
);
