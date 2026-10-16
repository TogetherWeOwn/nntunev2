-- NNTune v2 migration 0004: authoritative usage ledger.
--
-- PlanetScale Postgres via Hyperdrive. All access uses the parameterized
-- statements in workers/api/src/ledger.rs (never string-interpolated).
--
-- Analytics Engine carries the fast per-second/per-byte signal; this table
-- is the billing truth the usage endpoint and limit enforcement read:
-- one row per billed service window, replay-safe on `idempotency_key`
-- (a retried finalize inserts zero rows instead of double-billing).
-- `period_month` is `YYYY-MM` in UTC, derived from `recorded_at`, so
-- monthly usage is SUM over (user_id, period_month).

CREATE TABLE IF NOT EXISTS usage_ledger (
    id              BIGSERIAL PRIMARY KEY,
    idempotency_key TEXT NOT NULL UNIQUE CHECK (char_length(idempotency_key) BETWEEN 1 AND 128),
    user_id         TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    track_id        TEXT NOT NULL CHECK (char_length(track_id) BETWEEN 1 AND 128),
    period_month    TEXT NOT NULL CHECK (period_month ~ '^[0-9]{4}-(0[1-9]|1[0-2])$'),
    stream_seconds  INTEGER NOT NULL CHECK (stream_seconds >= 0),
    bytes_served    BIGINT NOT NULL CHECK (bytes_served >= 0),
    recorded_at     BIGINT NOT NULL CHECK (recorded_at >= 0),
    CHECK (stream_seconds > 0 OR bytes_served > 0)
);
CREATE INDEX IF NOT EXISTS idx_usage_ledger_user_period ON usage_ledger (user_id, period_month);
