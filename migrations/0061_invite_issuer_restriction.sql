SET search_path = public, pg_catalog;

-- No backfill. This is an invite-issuance restriction, not an account ban.
ALTER TABLE public.users ADD COLUMN IF NOT EXISTS invite_issuance_restricted_at BIGINT;
