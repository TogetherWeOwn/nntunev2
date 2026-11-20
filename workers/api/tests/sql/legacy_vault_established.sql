-- The migration test runner has already applied every migration. Recreate
-- the pre-expand shape and run the real candidate in a scratch transaction.
BEGIN;

ALTER TABLE public.users DROP COLUMN legacy_vault_established;

INSERT INTO public.users (id, email, created_at, role) VALUES
    ('legacy-marker-vault', 'legacy-marker-vault@example.com', 1, 'member'),
    ('legacy-marker-empty', 'legacy-marker-empty@example.com', 1, 'member');

-- Only the vault row establishes the first member: no passkey or session.
INSERT INTO public.provider_vaults (
    user_id, kek_version, dek_wrapped_nonce, dek_wrapped, payload_nonce, payload, updated_at
) VALUES (
    'legacy-marker-vault', 1, 'synthetic-nonce', 'synthetic-dek',
    'synthetic-nonce', 'synthetic-payload', 1
);

\ir ../../../../migrations/0048_legacy_vault_established.sql

DO $$
DECLARE
    fresh_marker BOOLEAN;
BEGIN
    IF (SELECT legacy_vault_established FROM public.users WHERE id = 'legacy-marker-vault')
       IS DISTINCT FROM TRUE THEN
        RAISE EXCEPTION 'pre-existing vault-only member was not marked';
    END IF;
    IF (SELECT legacy_vault_established FROM public.users WHERE id = 'legacy-marker-empty')
       IS DISTINCT FROM FALSE THEN
        RAISE EXCEPTION 'pre-existing no-vault member must remain unmarked';
    END IF;

    -- Omit the new column, just as the current Worker does on signup.
    INSERT INTO public.users (id, email, created_at)
    VALUES ('legacy-marker-fresh', 'legacy-marker-fresh@example.com', 2)
    RETURNING legacy_vault_established INTO fresh_marker;
    IF fresh_marker IS DISTINCT FROM FALSE THEN
        RAISE EXCEPTION 'fresh signup must default to unmarked';
    END IF;
END
$$;

\ir ../../../../migrations/0048_legacy_vault_established.sql

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM (VALUES ('legacy-marker-vault', TRUE),
                     ('legacy-marker-empty', FALSE),
                     ('legacy-marker-fresh', FALSE)) AS expected(id, marker)
        LEFT JOIN public.users AS u ON u.id = expected.id
        WHERE u.legacy_vault_established IS DISTINCT FROM expected.marker
    ) THEN
        RAISE EXCEPTION 'migration replay changed the expected markers';
    END IF;
END
$$;

ROLLBACK;
