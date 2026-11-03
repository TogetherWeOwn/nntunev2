-- NNTune v2 migration 0032: ledger stream_seconds floor.
--
-- Migration 0020's rate CHECK implicitly guaranteed `stream_seconds >= 1`
-- on every row (seconds were `ceil(bytes / 48000)` over a `bytes >= 1`
-- domain, so no zero-second row could ever be written). Migration 0031
-- retired that CHECK when the derivation moved to delivered-byte
-- proportion, leaving only the column's `>= 0` range CHECK — while the
-- api Worker's `prepare_ledger_row` still refuses zero seconds for
-- positive bytes (every derivation bills at least one second). This
-- migration restores the floor in the schema so a persisted row meets
-- the same rule the writer enforces: `[1, 86400]`.
--
-- Data safety: no existing row can violate the new CHECK — before 0031
-- the rate CHECK made zero seconds impossible, and since 0031 the only
-- writer path is the prepare boundary that rejects them.
--
-- Replay-safety (run_migration_tests.sh re-applies every migration):
-- the old range CHECK is dropped only when still present (found by its
-- definition, never by a guessed auto-generated name), and the new
-- CHECK is added only when absent.

DO $$
DECLARE
    range_check_name TEXT;
BEGIN
    SELECT conname INTO range_check_name
    FROM pg_constraint
    WHERE conrelid = 'public.stream_play_ledger'::regclass
      AND contype = 'c'
      AND pg_get_constraintdef(oid) LIKE '%stream_seconds >= 0%';
    IF range_check_name IS NOT NULL THEN
        EXECUTE format(
            'ALTER TABLE public.stream_play_ledger DROP CONSTRAINT %I',
            range_check_name
        );
    END IF;
END $$;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'public.stream_play_ledger'::regclass
          AND contype = 'c'
          AND pg_get_constraintdef(oid) LIKE '%stream_seconds >= 1%'
    ) THEN
        ALTER TABLE public.stream_play_ledger
            ADD CONSTRAINT stream_play_ledger_stream_seconds_bounds
            CHECK (stream_seconds >= 1 AND stream_seconds <= 86400);
    END IF;
END $$;
