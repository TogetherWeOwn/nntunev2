-- 0008_mb_import_resume_dlq: per-dump resume cursors + dead-letter queue.
BEGIN;
--
-- The cursor is scoped to a content hash, so a newer dump of any size starts
-- at line 0 while an interrupted identical dump resumes at its last chunk.
-- Existing cursors and DLQ entries are retained under the `legacy` key.
-- The upsert remains forward-only: older dump rows never roll back newer data.
-- Roles: importer owns these tables (same as 0001); the MB read-only role
-- gets NO grants here (cursor + DLQ are operator surfaces, never served).

-- First-seen dump line per row: written once on INSERT, never touched by
-- the upsert (a rerun skips completed chunks). Lets each chunk verify rows
-- it inserted from a dump.
ALTER TABLE mb_artist ADD COLUMN IF NOT EXISTS line_no BIGINT;
ALTER TABLE mb_release ADD COLUMN IF NOT EXISTS line_no BIGINT;
ALTER TABLE mb_recording ADD COLUMN IF NOT EXISTS line_no BIGINT;

-- Resume cursor: one row per curated dump table and content hash. Upgrade
-- the previous table-only cursor without discarding in-flight progress.
CREATE TABLE IF NOT EXISTS mb_import_cursor (
    table_name     TEXT NOT NULL CHECK (table_name IN ('artist', 'release', 'recording')),
    dump_id        TEXT NOT NULL DEFAULT 'legacy',
    lines_done     BIGINT NOT NULL DEFAULT 0 CHECK (lines_done >= 0),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (table_name, dump_id)
);
ALTER TABLE mb_import_cursor ADD COLUMN IF NOT EXISTS dump_id TEXT NOT NULL DEFAULT 'legacy';
ALTER TABLE mb_import_cursor DROP CONSTRAINT IF EXISTS mb_import_cursor_pkey;
ALTER TABLE mb_import_cursor ADD CONSTRAINT mb_import_cursor_pkey PRIMARY KEY (table_name, dump_id);

-- Legacy cursor row remains available for operator diagnosis. New imports
-- create a distinct hash-keyed cursor on demand.

-- Dead-letter queue: one row per poison line per dump, preserving distinct
-- failures at the same table/line offset in later dumps.
CREATE TABLE IF NOT EXISTS mb_import_dlq (
    id             BIGSERIAL PRIMARY KEY,
    table_name     TEXT NOT NULL CHECK (table_name IN ('artist', 'release', 'recording')),
    dump_id        TEXT NOT NULL DEFAULT 'legacy',
    line_no        BIGINT NOT NULL CHECK (line_no >= 1),
    reason         TEXT NOT NULL,
    raw_line       TEXT NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
ALTER TABLE mb_import_dlq ADD COLUMN IF NOT EXISTS dump_id TEXT NOT NULL DEFAULT 'legacy';
ALTER TABLE mb_import_dlq DROP CONSTRAINT IF EXISTS mb_import_dlq_table_name_line_no_key;
ALTER TABLE mb_import_dlq DROP CONSTRAINT IF EXISTS mb_import_dlq_table_dump_line_key;
ALTER TABLE mb_import_dlq ADD CONSTRAINT mb_import_dlq_table_dump_line_key
    UNIQUE (table_name, dump_id, line_no);

CREATE INDEX IF NOT EXISTS mb_import_dlq_table_reason_idx
    ON mb_import_dlq (table_name, reason);
COMMIT;
