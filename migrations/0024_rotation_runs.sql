-- Routine rewrap progress: same column order/types/defaults and partial
-- unique open-run index as ROTATION_RUNS_DDL in workers/api/src/rotation.rs.
-- Epoch BIGINT timestamps and application-generated TEXT IDs are intentional:
-- no SQLite-specific conversion or Postgres timestamp/UUID coercion is needed.
-- Staging first. Production application is a separate gated promotion.
CREATE TABLE IF NOT EXISTS rotation_runs (
  id             TEXT PRIMARY KEY,
  env            TEXT NOT NULL,
  from_version   INTEGER NOT NULL,
  to_version     INTEGER NOT NULL,
  cursor         TEXT NULL,
  rows_done      BIGINT NOT NULL DEFAULT 0,
  rows_failed    BIGINT NOT NULL DEFAULT 0,
  rows_deferred  BIGINT NOT NULL DEFAULT 0,
  claims_retried BIGINT NOT NULL DEFAULT 0,
  started_at     BIGINT NOT NULL,
  updated_at     BIGINT NOT NULL,
  finished_at    BIGINT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_rotation_runs_open
  ON rotation_runs (env, to_version) WHERE finished_at IS NULL;

-- A unique open run is not an invocation lock: two Workers can resume it.
-- This database-scoped singleton fences every short rewrap transaction.
-- PostgreSQL clock_timestamp() and SELECT ... FOR UPDATE are used by the
-- store, not embedded here, so the DDL retains the fixture SQL overlap.
CREATE TABLE IF NOT EXISTS rotation_state (
  id         TEXT PRIMARY KEY,
  token      TEXT NOT NULL,
  expires_at BIGINT NOT NULL
);
