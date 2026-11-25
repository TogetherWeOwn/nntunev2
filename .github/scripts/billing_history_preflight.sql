BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY;
SET LOCAL search_path = pg_catalog, pg_temp;
SET LOCAL statement_timeout = '5s';
SET LOCAL lock_timeout = '1s';
SET LOCAL row_security = off;

-- Admit the actual session before reading even the prepared evidence.
SELECT current_database() = :'database'
   AND current_user = :'observer' AND session_user = :'observer'
   AND EXISTS (SELECT 1 FROM pg_roles WHERE rolname = current_user AND rolcanlogin
               AND NOT (rolsuper OR rolbypassrls OR rolcreaterole OR rolcreatedb OR rolreplication))
   AND (SELECT count(*) = 2 AND bool_and(relkind = 'r' AND NOT relrowsecurity
           AND NOT has_table_privilege(oid, 'INSERT, UPDATE, DELETE, TRUNCATE, TRIGGER')
           AND NOT has_any_column_privilege(oid, 'INSERT, UPDATE'))
        FROM pg_class WHERE oid IN (to_regclass('public.subscriptions'),
                                    to_regclass('public.paid_invoice_history'))) AS admitted
\gset
\if :admitted
\else
ROLLBACK;
\quit
\endif

-- The admitted inventory is independent of the fold output. Projections may
-- be missing, but must be unique and may not silently expand the inventory.
WITH scope AS MATERIALIZED (
    SELECT stripe_invoice_id FROM billing_preflight.scope LIMIT :limit
), projection AS MATERIALIZED (
    SELECT stripe_invoice_id FROM billing_preflight.projection LIMIT :limit
)
SELECT (SELECT count(*) = :expected AND count(DISTINCT stripe_invoice_id) = :expected
        FROM scope)
   AND (SELECT count(*) = count(DISTINCT stripe_invoice_id) FROM projection)
   AND NOT EXISTS (SELECT stripe_invoice_id FROM projection EXCEPT SELECT stripe_invoice_id FROM scope)
   AS scope_complete
\gset
\if :scope_complete
\else
ROLLBACK;
\echo scope_incomplete
\quit
\endif

WITH checked AS MATERIALIZED (
    SELECT s.stripe_invoice_id, s.user_id, s.stripe_subscription_id,
           p.plan_id, p.start_s, p.end_s, p.amount_cents, p.currency, p.paid_s, p.starts, p.ends,
           (p.evidence_complete AND p.user_id = s.user_id
            AND p.stripe_subscription_id = s.stripe_subscription_id
            AND s.stripe_invoice_id <> '' AND s.user_id <> '' AND s.stripe_subscription_id <> ''
            AND p.plan_id <> '' AND p.start_s <= p.end_s
            AND p.start_s >= 0 AND p.end_s <= 253402300799
            AND p.paid_s BETWEEN 0 AND 253402300799
            AND p.amount_cents >= 0 AND p.currency ~ '^[a-z]{3}$'
            AND cardinality(p.starts) = cardinality(p.ends)
            AND cardinality(p.starts) <= 1000
            AND NOT EXISTS (SELECT 1 FROM unnest(p.starts, p.ends) AS b(lo, hi)
                            WHERE lo IS NULL OR hi IS NULL OR lo < 0
                               OR hi > 253402300799 OR lo >= hi)) IS TRUE AS usable,
           s.retained
    FROM billing_preflight.scope AS s
    LEFT JOIN billing_preflight.projection AS p USING (stripe_invoice_id)
), normalized AS MATERIALIZED (
    SELECT i.*,
           CASE WHEN usable THEN to_timestamp(i.start_s::bigint) END AS period_start,
           CASE WHEN usable THEN to_timestamp(i.end_s::bigint) END AS period_end,
           CASE WHEN usable THEN to_timestamp(i.paid_s::bigint) END AS paid_at,
           CASE WHEN usable THEN @COVERAGE@ END AS covered_periods
    FROM checked AS i
), verdicts AS (
    SELECT CASE
        WHEN NOT i.usable OR t.stripe_subscription_id IS NULL THEN 'incomplete'
        WHEN t.user_id IS DISTINCT FROM i.user_id THEN 'collision'
        WHEN i.retained IS DISTINCT FROM (history.stripe_invoice_id IS NOT NULL) THEN 'incomplete'
        WHEN history.stripe_invoice_id IS NULL THEN 'fresh'
        WHEN (@HYDRATION@) IS TRUE THEN 'eligible_hydration'
        WHEN EXISTS (@EXACT_REPLAY@) THEN 'exact'
        ELSE 'collision'
    END AS verdict
    FROM normalized AS i
    LEFT JOIN public.subscriptions AS t ON t.stripe_subscription_id = i.stripe_subscription_id
    LEFT JOIN public.paid_invoice_history AS history ON history.stripe_invoice_id = i.stripe_invoice_id
)
SELECT jsonb_build_object(
    'total', count(*), 'exact', count(*) FILTER (WHERE verdict = 'exact'),
    'eligible_hydration', count(*) FILTER (WHERE verdict = 'eligible_hydration'),
    'collision', count(*) FILTER (WHERE verdict = 'collision'),
    'incomplete', count(*) FILTER (WHERE verdict = 'incomplete'),
    'fresh', count(*) FILTER (WHERE verdict = 'fresh')
)
FROM verdicts;
ROLLBACK;
