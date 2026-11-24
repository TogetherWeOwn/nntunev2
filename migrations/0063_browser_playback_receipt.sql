SET search_path = public, pg_catalog;

-- One server-owned receipt per account lease. A legacy lease can supersede
-- a receipt, whose play ID and epoch must always match before it is read.
ALTER TABLE public.playback_leases
    ADD COLUMN IF NOT EXISTS browser_receipt jsonb;
