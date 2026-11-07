-- Durable bounded-work cursor within a generation's scheduled recheck pass.
-- This is resume bookkeeping, not negative health or content/cleanup proof.
-- Existing schedules begin at slot zero without resetting their pass budget.
SET search_path = public, pg_catalog;

ALTER TABLE posted_file_recheck_schedule
    ADD COLUMN IF NOT EXISTS work_cursor INTEGER NOT NULL DEFAULT 0
    CHECK (work_cursor >= 0);
