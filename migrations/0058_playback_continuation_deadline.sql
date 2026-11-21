SET search_path = public, pg_catalog;

-- NULL identifies a lease issued by a Worker without a per-play deadline.
ALTER TABLE public.playback_leases
    ADD COLUMN IF NOT EXISTS continuation_deadline BIGINT
        CHECK (continuation_deadline BETWEEN 0 AND 253402300799);
