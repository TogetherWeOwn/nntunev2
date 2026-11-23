SET search_path = public, pg_catalog;

-- No backfill: expiry is recorded only when an authoritative caller observes it.
ALTER TABLE public.invite_codes ADD COLUMN IF NOT EXISTS expiry_observed_at BIGINT;
