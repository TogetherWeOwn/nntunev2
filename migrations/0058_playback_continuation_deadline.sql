SET search_path = public, pg_catalog;

-- An older Worker can replace the lease without updating either column.
-- The deadline applies only when its issuing epoch matches the lease epoch.
ALTER TABLE public.playback_leases
    ADD COLUMN IF NOT EXISTS continuation_deadline BIGINT
        CHECK (continuation_deadline BETWEEN 0 AND 253402300799),
    ADD COLUMN IF NOT EXISTS continuation_epoch BIGINT;
