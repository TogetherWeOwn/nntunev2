-- NNTune v2 migration 0009: durable metered admission ledger.
--
-- PlanetScale Postgres via Hyperdrive. The ingest edge owns one row per
-- admitted (job_id, stage): `pending` from first record until the Queue
-- send is accepted and the entry `Ok` meter event is written, then
-- `queued`. The PRIMARY KEY is the concurrency authority: concurrent
-- record inserts converge (one row wins; every loser reads the same
-- mark), which is what makes the entry meter idempotent across client
-- retries. Statements live in workers/ingest/src/lib.rs
-- (`admission_ensure_sql` / `admission_claim_sql` /
-- `admission_read_mark_sql` / `admission_mark_queued_sql` /
-- `admission_release_sql`); this file only creates the table.
-- Analytics Engine stays a projection: it is never read for dedupe.
--
-- Apply out of band (staging first) before the worker build that records
-- admissions rolls out; until then admission fails closed (503), never
-- silently.

CREATE TABLE IF NOT EXISTS metered_admission (
    job_id  TEXT NOT NULL,
    stage   TEXT NOT NULL,
    state   TEXT NOT NULL DEFAULT 'pending',
    PRIMARY KEY (job_id, stage)
);
