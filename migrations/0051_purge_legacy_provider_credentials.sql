-- Contract only after deployed consumers have drained. These drops are one-way.
SET search_path = public, pg_catalog;

DO $purge$
BEGIN
    -- Freeze all guard inputs through the drops. Do not queue behind an
    -- active writer; a lock failure leaves the migration unrecorded.
    LOCK TABLE public.user_cache_grants, public.providers,
        public.provider_vaults, public.vault_test_leases
        IN ACCESS EXCLUSIVE MODE NOWAIT;
    LOCK TABLE public.users, public.sessions, public.webauthn_credentials
        IN SHARE MODE NOWAIT;

    IF EXISTS (SELECT 1 FROM public.user_cache_grants) THEN
        RAISE EXCEPTION 'legacy provider purge refused: cache grants are not empty';
    END IF;

    -- Preserve the non-secret evidence before destroying its source. A
    -- marker without a surviving vault is intentionally not cleared.
    IF EXISTS (
        SELECT 1 FROM public.provider_vaults AS v
        JOIN public.users AS u ON u.id = v.user_id
        WHERE NOT u.legacy_vault_established
    ) THEN
        RAISE EXCEPTION 'legacy provider purge refused: a vault owner lacks the account marker';
    END IF;

    -- The providers FK currently prevents reaping these users. Do not
    -- count temporary protections (age, open challenges or magic links).
    -- Match the enduring conditions in unverified_user_filter, including
    -- any session, even one that is revoked. Never admit users via a provider.
    IF EXISTS (
        SELECT 1 FROM public.providers AS p
        JOIN public.users AS u ON u.id = p.user_id
        WHERE u.role = 'member'
          AND u.deleting_at IS NULL
          AND NOT u.legacy_vault_established
          AND NOT EXISTS (SELECT 1 FROM public.sessions AS s WHERE s.user_id = u.id)
          AND NOT EXISTS (SELECT 1 FROM public.webauthn_credentials AS c WHERE c.user_id = u.id)
    ) THEN
        RAISE EXCEPTION 'legacy provider purge refused: provider owners would lose reaping protection';
    END IF;

    -- Drop the referencing table first. RESTRICT leaves unexpected
    -- dependants intact and aborts all drops in this transaction.
    DROP TABLE public.user_cache_grants RESTRICT;
    DROP TABLE public.providers RESTRICT;
    DROP TABLE public.provider_vaults RESTRICT;
    DROP TABLE public.vault_test_leases RESTRICT;
END;
$purge$;
