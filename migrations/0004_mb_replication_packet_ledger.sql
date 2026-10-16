-- 0004_mb_replication_packet_ledger: durable per-packet SLO evidence.
-- One row per published/applied packet; sequence gaps remain observable.
CREATE TABLE IF NOT EXISTS mb_replication_packet (
    sequence      BIGINT PRIMARY KEY CHECK (sequence >= 0),
    packet_id     TEXT NOT NULL UNIQUE,
    published_at  TIMESTAMPTZ NOT NULL,
    applied_at    TIMESTAMPTZ NOT NULL,
    lag_secs      BIGINT NOT NULL CHECK (lag_secs >= 0),
    recorded_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (applied_at >= published_at)
);

CREATE INDEX IF NOT EXISTS mb_replication_packet_applied_at_idx
    ON mb_replication_packet (applied_at);
