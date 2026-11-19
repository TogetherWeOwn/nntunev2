-- Preserve the vault-existence cohort for a later admission cutover.
-- This is a snapshot, not a live mirror: reconcile membership before cutover.
SET search_path = public, pg_catalog;

ALTER TABLE public.users
    ADD COLUMN IF NOT EXISTS legacy_vault_established BOOLEAN NOT NULL DEFAULT FALSE;

UPDATE public.users AS u
SET legacy_vault_established = TRUE
WHERE NOT u.legacy_vault_established
  AND EXISTS (
      SELECT 1 FROM public.provider_vaults AS v WHERE v.user_id = u.id
  );
