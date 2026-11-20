-- Persist admission order independently of caller timestamps and job lifetime.
-- This is preparation for curated source replacement, not a membership grant
-- or a conversion of legacy source ciphertext/media to service-owned material.
SET search_path = public, pg_catalog;

DO $$
DECLARE
    last_historical_seq BIGINT;
BEGIN
    -- One DDL transaction holds the table lock through historical numbering,
    -- identity installation and sequence initialization. Replays never renumber
    -- jobs or rewind past values consumed by rolled-back/conflicting inserts.
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'ingest_jobs'
          AND column_name = 'accepted_seq'
    ) THEN
        ALTER TABLE public.ingest_jobs ADD COLUMN accepted_seq BIGINT;
        WITH historical AS (
            SELECT id, row_number() OVER (ORDER BY accepted_at, id) AS seq
            FROM public.ingest_jobs
        )
        UPDATE public.ingest_jobs j SET accepted_seq = h.seq
        FROM historical h WHERE j.id = h.id;

        ALTER TABLE public.ingest_jobs ALTER COLUMN accepted_seq SET NOT NULL;
        ALTER TABLE public.ingest_jobs ALTER COLUMN accepted_seq ADD GENERATED ALWAYS AS IDENTITY;
        ALTER TABLE public.ingest_jobs ADD CONSTRAINT ingest_jobs_accepted_seq_key UNIQUE (accepted_seq);
        SELECT max(accepted_seq) INTO last_historical_seq FROM public.ingest_jobs;
        PERFORM setval(pg_get_serial_sequence('public.ingest_jobs', 'accepted_seq'),
                       coalesce(last_historical_seq, 1), last_historical_seq IS NOT NULL);
    END IF;
END
$$;
