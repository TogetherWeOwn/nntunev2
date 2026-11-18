-- Repeat the 0048 backfill just before the separate admission cutover.
UPDATE public.users AS u
SET legacy_vault_established = TRUE
WHERE NOT u.legacy_vault_established
  AND EXISTS (
      SELECT 1 FROM public.provider_vaults AS v WHERE v.user_id = u.id
  );

-- One statement observes both directions in the same snapshot, including
-- this transaction's update. The apply receipt does not certify later writes.
DO $$
DECLARE
    vault_without_marker BOOLEAN;
    marker_without_vault BOOLEAN;
BEGIN
    SELECT EXISTS (
               SELECT 1 FROM public.users AS u
               WHERE NOT u.legacy_vault_established
                 AND EXISTS (SELECT 1 FROM public.provider_vaults AS v WHERE v.user_id = u.id)
           ), EXISTS (
               SELECT 1 FROM public.users AS u
               WHERE u.legacy_vault_established
                 AND NOT EXISTS (SELECT 1 FROM public.provider_vaults AS v WHERE v.user_id = u.id)
           )
    INTO vault_without_marker, marker_without_vault;

    IF marker_without_vault THEN
        RAISE EXCEPTION USING ERRCODE = 'P0049', MESSAGE = CASE
            WHEN vault_without_marker THEN 'legacy vault cohort drift: marker without vault and vault without marker'
            ELSE 'legacy vault cohort drift: marker without vault'
        END;
    ELSIF vault_without_marker THEN
        RAISE EXCEPTION 'legacy vault cohort drift: vault without marker';
    END IF;
END
$$;
