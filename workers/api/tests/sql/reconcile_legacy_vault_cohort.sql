-- The runner has applied all migrations to an empty database. Recreate
-- vault ownership on either side of the original backfill in scratch data.
BEGIN;

INSERT INTO public.users (id, email, created_at) VALUES
    ('cohort-existing', 'cohort-existing@example.com', 1),
    ('cohort-late', 'cohort-late@example.com', 1),
    ('cohort-empty', 'cohort-empty@example.com', 1);

INSERT INTO public.provider_vaults (
    user_id, kek_version, dek_wrapped_nonce, dek_wrapped, payload_nonce, payload, updated_at
) VALUES ('cohort-existing', 1, 'synthetic-nonce', 'synthetic-dek',
          'synthetic-nonce', 'synthetic-payload', 1);

\ir ../../../../migrations/0048_legacy_vault_established.sql

INSERT INTO public.provider_vaults (
    user_id, kek_version, dek_wrapped_nonce, dek_wrapped, payload_nonce, payload, updated_at
) VALUES ('cohort-late', 1, 'synthetic-nonce', 'synthetic-dek',
          'synthetic-nonce', 'synthetic-payload', 2);

CREATE FUNCTION pg_temp.assert_legacy_vault_cohort() RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM (VALUES ('cohort-existing', TRUE), ('cohort-late', TRUE),
                     ('cohort-empty', FALSE)) AS expected(id, marker)
        LEFT JOIN public.users AS u ON u.id = expected.id
        WHERE u.legacy_vault_established IS DISTINCT FROM expected.marker
    ) THEN
        RAISE EXCEPTION 'reconciliation or replay left incorrect cohort markers';
    END IF;
END
$$;

SAVEPOINT before_reconciliation;
\ir ../../../../migrations/0049_reconcile_legacy_vault_cohort.sql
SELECT pg_temp.assert_legacy_vault_cohort();
\ir ../../../../migrations/0049_reconcile_legacy_vault_cohort.sql
SELECT pg_temp.assert_legacy_vault_cohort();

-- Rewind the successful runs. The candidate now has both a late vault to
-- mark and an established user who lost their vault. Only the latter must
-- cause refusal; the late user's intermediate update must roll back.
ROLLBACK TO SAVEPOINT before_reconciliation;
DELETE FROM public.provider_vaults WHERE user_id = 'cohort-existing';
SAVEPOINT before_refusal;
\set ON_ERROR_STOP off
\ir ../../../../migrations/0049_reconcile_legacy_vault_cohort.sql
\set reconciliation_sqlstate :SQLSTATE
\set ON_ERROR_STOP on
ROLLBACK TO SAVEPOINT before_refusal;

SELECT :'reconciliation_sqlstate' = 'P0049' AS refused_reverse_drift \gset
\if :refused_reverse_drift
\else
    DO $$ BEGIN RAISE EXCEPTION 'expected marker-without-vault refusal'; END $$;
\endif

DO $$
BEGIN
    IF (SELECT legacy_vault_established FROM public.users WHERE id = 'cohort-existing')
           IS DISTINCT FROM TRUE
       OR (SELECT legacy_vault_established FROM public.users WHERE id = 'cohort-late')
           IS DISTINCT FROM FALSE THEN
        RAISE EXCEPTION 'refusal must leave the original markers unchanged';
    END IF;
END
$$;

ROLLBACK;
