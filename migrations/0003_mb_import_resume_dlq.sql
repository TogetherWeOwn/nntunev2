-- 0003_mb_import_resume_dlq: kill-safe import cursor + dead-letter queue.
--
-- What: per-table resume cursor (`mb_import_cursor`) so a killed import
-- restarts after its last committed chunk, and a dead-letter table
-- (`mb_import_dlq`) for poison rows with reason + raw line for operators.
-- The upsert itself stays forward-only (same WHERE guard as before):
-- rerunning over the same dump moves zero rows.
-- Out of scope: search tuning, genre/playlist, scheduling UI.
-- Roles: importer owns these tables (same as 0001); the MB read-only
-- role gets NO grants here (cursor + DLQ are operator surfaces, never
-- served to interactive traffic).

-- First-seen dump line per row: written once on INSERT, never touched by
-- the upsert (a rerun skips completed chunks, so a newer dump keeps the
-- original line). Lets the operator trace any row back to its dump line
-- and lets each chunk verify exactly the rows it committed.
ALTER TABLE mb_artist ADD COLUMN IF NOT EXISTS line_no BIGINT;
ALTER TABLE mb_release ADD COLUMN IF NOT EXISTS line_no BIGINT;
ALTER TABLE mb_recording ADD COLUMN IF NOT EXISTS line_no BIGINT;

-- Resume cursor: one row per curated dump table, lines already committed.
CREATE TABLE IF NOT EXISTS mb_import_cursor (
    table_name     TEXT PRIMARY KEY CHECK (table_name IN ('artist', 'release', 'recording')),
    lines_done     BIGINT NOT NULL DEFAULT 0 CHECK (lines_done >= 0),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO mb_import_cursor (table_name, lines_done)
    VALUES ('artist', 0), ('release', 0), ('recording', 0)
    ON CONFLICT (table_name) DO NOTHING;

-- Dead-letter queue: one row per poison dump line. `raw_line` keeps the
-- exact bytes (a later COPY of the validator's escaped output round-trips
-- them); `reason` is a lowercase snake_case validator code.
CREATE TABLE IF NOT EXISTS mb_import_dlq (
    id             BIGSERIAL PRIMARY KEY,
    table_name     TEXT NOT NULL CHECK (table_name IN ('artist', 'release', 'recording')),
    line_no        BIGINT NOT NULL CHECK (line_no >= 1),
    reason         TEXT NOT NULL,
    raw_line       TEXT NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (table_name, line_no)
);

CREATE INDEX IF NOT EXISTS mb_import_dlq_table_reason_idx
    ON mb_import_dlq (table_name, reason);
