-- Run on the disposable database after all migrations. Seed a pre-existing
-- marker, a vault that arrived after reconciliation, and a vaultless user.
BEGIN;

INSERT INTO public.users (id, email, created_at, legacy_vault_established) VALUES
    ('refresh-existing', 'refresh-existing@example.com', 1, TRUE),
    ('refresh-late', 'refresh-late@example.com', 1, FALSE),
    ('refresh-empty', 'refresh-empty@example.com', 1, FALSE);

INSERT INTO public.provider_vaults (
    user_id, kek_version, dek_wrapped_nonce, dek_wrapped, payload_nonce, payload, updated_at
) VALUES
    ('refresh-existing', 1, 'synthetic-nonce', 'synthetic-dek', 'synthetic-nonce', 'synthetic-payload', 1),
    ('refresh-late', 1, 'synthetic-nonce', 'synthetic-dek', 'synthetic-nonce', 'synthetic-payload', 2);

CREATE FUNCTION pg_temp.assert_refresh_cohort(late_marker BOOLEAN) RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM (VALUES ('refresh-existing', TRUE), ('refresh-late', late_marker),
                     ('refresh-empty', FALSE)) AS expected(id, marker)
        LEFT JOIN public.users AS u ON u.id = expected.id
        WHERE u.legacy_vault_established IS DISTINCT FROM expected.marker
    ) THEN
        RAISE EXCEPTION 'unexpected marker after refresh, replay or rollback';
    END IF;
END
$$;

SAVEPOINT before_refresh;
\ir ../../../../migrations/0050_refresh_legacy_vault_cohort.sql
SELECT pg_temp.assert_refresh_cohort(TRUE);
\ir ../../../../migrations/0050_refresh_legacy_vault_cohort.sql
SELECT pg_temp.assert_refresh_cohort(TRUE);

-- The refresh must refuse a marker with no corresponding vault, and its
-- repair of the other user's late vault must roll back with the refusal.
ROLLBACK TO SAVEPOINT before_refresh;
DELETE FROM public.provider_vaults WHERE user_id = 'refresh-existing';
SAVEPOINT before_refusal;
\set ON_ERROR_STOP off
\ir ../../../../migrations/0050_refresh_legacy_vault_cohort.sql
\set refresh_sqlstate :SQLSTATE
\set ON_ERROR_STOP on
ROLLBACK TO SAVEPOINT before_refusal;

SELECT :'refresh_sqlstate' = 'P0050' AS refused_reverse_drift \gset
\if :refused_reverse_drift
\else
    DO $$ BEGIN RAISE EXCEPTION 'expected marker-without-vault refusal'; END $$;
\endif
SELECT pg_temp.assert_refresh_cohort(FALSE);

\echo vault cohort refresh: late vault repaired, replay stable, reverse drift refused and rolled back
ROLLBACK;
