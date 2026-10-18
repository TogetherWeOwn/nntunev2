-- NNTune v2 migration 0019: authoritative per-play stream ledger.
--
-- PlanetScale Postgres via Hyperdrive. All access uses the parameterized
-- statements in workers/api/src/ledger.rs (never string-interpolated).
--
-- Analytics Engine carries the fast per-second/per-byte signal; this table
-- is the billing truth the usage endpoint and limit enforcement read:
-- one row per billed play window, replay-safe on `idempotency_key`
-- (a retried finalize inserts zero rows instead of double-billing, then the
-- Worker SELECTs the stored row and classifies exact redelivery vs conflict).
-- `period_month` is `YYYY-MM` in UTC, derived from `recorded_at`, so
-- monthly usage is SUM over (user_id, period_month).
--
-- Table identity: this is NOT the accepted ADR 0002/0004 daily `usage_ledger`
-- (`period DATE`, `ingest_jobs`, `UNIQUE (user_id, period)`). That daily
-- aggregate is built in a later slice by folding these per-play windows. The
-- distinct name keeps the future `CREATE TABLE IF NOT EXISTS usage_ledger`
-- from silently no-op'ing on a colliding shape. Stream-play keys are
-- `play:{user}:{track}:{play}` (up to 1159 chars, alphabet
-- `[A-Za-z0-9_:-]`); fresh random windows use 22-char base64url handles.

CREATE TABLE IF NOT EXISTS stream_play_ledger (
    id              BIGSERIAL PRIMARY KEY,
    idempotency_key TEXT NOT NULL UNIQUE CHECK (char_length(idempotency_key) BETWEEN 1 AND 1159) CHECK (idempotency_key ~ '^[A-Za-z0-9_:-]+$'),
    user_id         TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE CHECK (char_length(user_id) BETWEEN 1 AND 128),
    track_id        TEXT NOT NULL CHECK (char_length(track_id) BETWEEN 1 AND 512),
    period_month    TEXT NOT NULL CHECK (period_month ~ '^[0-9]{4}-(0[1-9]|1[0-2])$'),
    stream_seconds  INTEGER NOT NULL CHECK (stream_seconds >= 0 AND stream_seconds <= 86400),
    bytes_served    BIGINT NOT NULL CHECK (bytes_served >= 0 AND bytes_served <= 4147200000),
    CHECK (
        stream_seconds = (bytes_served / 48000) +
            CASE WHEN bytes_served % 48000 = 0 THEN 0 ELSE 1 END
    ),
    recorded_at     BIGINT NOT NULL CHECK (recorded_at >= 0 AND recorded_at <= 253402300799),
    CHECK (stream_seconds > 0 OR bytes_served > 0)
);
CREATE INDEX IF NOT EXISTS idx_stream_play_ledger_user_period ON stream_play_ledger (user_id, period_month);
