-- Regression test for the billing reconcile sweeps' bounded, fair progress.
-- Run after ALL migrations are applied (see ../run_migration_tests.sh). The
-- production claim SQL (src/billing_subscription_sweep.sql and
-- src/billing_checkout_sweep.sql) is injected by the runner through the psql
-- variables billing_subscription_sweep_sql / billing_checkout_sweep_sql and
-- becomes the body of a pg_temp function, so the test exercises the exact
-- text the worker binds ($1 = claim limit). Nothing is duplicated here.
-- Everything runs in one transaction that is rolled back; any broken
-- expectation RAISEs and (with ON_ERROR_STOP) fails the run. No sleeps:
-- ordering relies on NULLS FIRST, a fixed stale timestamp, and the monotonic
-- clock_timestamp() stamps of successive claims.
--
-- Fixture (subscriptions and open billing_operations alike):
--   * 100 "leaders" with reconcile_attempted_at NULL, ids 001..100;
--   * 20 "tail" rows (ids 101..120) already attempted at a fixed old time,
--     so they are due but sort after every NULL;
--   * ineligible rows with LOWER ids than all eligible ones (so only the
--     WHERE clause can exclude them) that must stay untouched.
-- Claims are never followed by any remote or state update, modelling a
-- sweep whose every candidate keeps failing.

BEGIN;

CREATE FUNCTION pg_temp.claim_subscriptions(bigint)
RETURNS TABLE (user_id text, stripe_subscription_id text)
LANGUAGE sql AS :'billing_subscription_sweep_sql';

CREATE FUNCTION pg_temp.claim_checkouts(bigint)
RETURNS TABLE (id text, user_id text, kind text, state text,
               stripe_subscription_id text, request_payload text)
LANGUAGE sql AS :'billing_checkout_sweep_sql';

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM subscriptions)
       OR EXISTS (SELECT 1 FROM billing_operations) THEN
        RAISE EXCEPTION 'sweep fixture needs empty subscriptions and billing_operations';
    END IF;
END $$;

-- ---------------------------------------------------------------- fixture
INSERT INTO users (id, email, created_at)
SELECT 'user-sw-sub-' || lpad(i::text, 3, '0'),
       'sw-sub-' || i || '@example.com', 1700000000
FROM generate_series(1, 125) AS i;
INSERT INTO users (id, email, created_at)
SELECT 'user-sw-op-' || lpad(i::text, 3, '0'),
       'sw-op-' || i || '@example.com', 1700000000
FROM generate_series(1, 125) AS i;

-- Eligible subscriptions 001..120: status cycles through every swept status.
-- active rows carry a far-future period end, paused rows a NULL one, so the
-- sweep must not key off coverage. Paid dates / plan / grace are non-trivial
-- so state preservation is observable.
INSERT INTO subscriptions
    (id, user_id, stripe_subscription_id, stripe_customer_id,
     stripe_subscription_item_id, stripe_price_id, plan_id, status,
     current_period_end, cancel_at_period_end, trial_end, paid_through_at,
     paid_invoice_id, grace_until, reconcile_attempted_at)
SELECT ('00000000-0000-0000-0002-' || lpad(i::text, 12, '0'))::uuid,
       'user-sw-sub-' || lpad(i::text, 3, '0'),
       'sub_sw_' || lpad(i::text, 3, '0'),
       'cus_sw_' || lpad(i::text, 3, '0'),
       'si_sw_' || lpad(i::text, 3, '0'),
       'price_sw',
       'plus',
       (ARRAY['incomplete', 'active', 'past_due', 'unpaid', 'paused'])[1 + i % 5],
       CASE i % 5 WHEN 1 THEN '2999-01-01 00:00:00+00'::timestamptz
                  WHEN 4 THEN NULL
                  ELSE '2026-01-01 00:00:00+00'::timestamptz END,
       (i % 7 = 0),
       CASE WHEN i % 3 = 0 THEN '2026-02-01 00:00:00+00'::timestamptz END,
       '2026-03-01 00:00:00+00'::timestamptz + (i || ' minutes')::interval,
       'in_sw_' || i,
       CASE WHEN i % 2 = 0 THEN '2026-04-01 00:00:00+00'::timestamptz END,
       CASE WHEN i > 100 THEN '2000-01-01 00:00:00+00'::timestamptz END
FROM generate_series(1, 120) AS i;

-- Terminal subscriptions (lower ids, NULL attempt stamp): never claimed.
INSERT INTO subscriptions
    (id, user_id, stripe_subscription_id, stripe_customer_id,
     stripe_subscription_item_id, stripe_price_id, plan_id, status)
SELECT ('00000000-0000-0000-0001-' || lpad(i::text, 12, '0'))::uuid,
       'user-sw-sub-' || lpad((120 + i)::text, 3, '0'),
       'sub_sw_term_' || i, 'cus_sw_term_' || i, 'si_sw_term_' || i,
       'price_sw', 'plus',
       CASE WHEN i % 2 = 0 THEN 'canceled' ELSE 'incomplete_expired' END
FROM generate_series(1, 5) AS i;

-- Open operations 001..120: state cycles; i % 3 picks sessionless (no
-- subscription, empty payload), expired (past pending_update_expires_at) or
-- missing (subscription id with no subscriptions row).
INSERT INTO billing_operations
    (id, user_id, kind, state, request_key, request_payload,
     stripe_subscription_id, pending_update_expires_at,
     reconcile_attempted_at)
SELECT ('00000000-0000-0000-0002-' || lpad(i::text, 12, '0'))::uuid,
       'user-sw-op-' || lpad(i::text, 3, '0'),
       CASE i % 2 WHEN 0 THEN 'checkout' ELSE 'plan_change' END,
       (ARRAY['pending', 'submitted', 'needs_reconcile'])[1 + i % 3],
       'req-sw-op-' || i,
       CASE i % 3
           WHEN 0 THEN '{}'::jsonb
           ELSE jsonb_build_object('plan', 'plus', 'n', i,
                                   'nested', jsonb_build_object('k', 'v ' || i))
       END,
       CASE i % 3 WHEN 0 THEN NULL ELSE 'sub_sw_missing_' || i END,
       CASE i % 3 WHEN 2 THEN '2000-01-01 00:00:00+00'::timestamptz END,
       CASE WHEN i > 100 THEN '2000-01-01 00:00:00+00'::timestamptz END
FROM generate_series(1, 120) AS i;

-- Resolved operations (lower ids, NULL attempt stamp): never claimed.
INSERT INTO billing_operations
    (id, user_id, kind, state, request_key, request_payload,
     stripe_subscription_id)
SELECT ('00000000-0000-0000-0001-' || lpad(i::text, 12, '0'))::uuid,
       'user-sw-op-' || lpad((120 + i)::text, 3, '0'),
       'checkout',
       CASE WHEN i % 2 = 0 THEN 'done' ELSE 'superseded' END,
       'req-sw-op-done-' || i, '{"x": 1}'::jsonb, 'sub_sw_done_' || i
FROM generate_series(1, 5) AS i;

-- Snapshots of everything the sweeps must NOT change (all columns but the
-- attempt stamp) plus usage state (no grants from sweeping).
CREATE TEMP TABLE sw_sub_before AS
SELECT id, to_jsonb(s) - 'reconcile_attempted_at' AS body FROM subscriptions s;
CREATE TEMP TABLE sw_op_before AS
SELECT id, to_jsonb(o) - 'reconcile_attempted_at' AS body FROM billing_operations o;
CREATE TEMP TABLE sw_usage_before AS
SELECT count(*) AS n FROM stream_play_ledger;

-- ------------------------------------------------------- subscription sweep
CREATE TEMP TABLE sw_sub_c1 AS SELECT * FROM pg_temp.claim_subscriptions(50);
CREATE TEMP TABLE sw_sub_c2 AS SELECT * FROM pg_temp.claim_subscriptions(50);
-- Remember attempt stamps before the third claim to prove they advance.
CREATE TEMP TABLE sw_sub_pre3 AS
SELECT stripe_subscription_id, reconcile_attempted_at AS prev
FROM subscriptions;
CREATE TEMP TABLE sw_sub_c3 AS SELECT * FROM pg_temp.claim_subscriptions(50);

DO $$
DECLARE n bigint;
BEGIN
    IF (SELECT count(*) FROM sw_sub_c1) <> 50
       OR (SELECT count(*) FROM sw_sub_c2) <> 50
       OR (SELECT count(*) FROM sw_sub_c3) <> 50 THEN
        RAISE EXCEPTION 'each subscription claim must return exactly the limit (50)';
    END IF;

    -- Claims 1 and 2 are disjoint and together cover every NULL leader,
    -- in id order (001..050, then 051..100).
    IF EXISTS (SELECT 1 FROM sw_sub_c1 JOIN sw_sub_c2 USING (stripe_subscription_id)) THEN
        RAISE EXCEPTION 'subscription claims 1 and 2 overlap';
    END IF;
    IF (SELECT array_agg(stripe_subscription_id ORDER BY stripe_subscription_id)
        FROM sw_sub_c1)
       IS DISTINCT FROM (SELECT array_agg('sub_sw_' || lpad(i::text, 3, '0') ORDER BY i)
                         FROM generate_series(1, 50) AS i)
       OR (SELECT array_agg(stripe_subscription_id ORDER BY stripe_subscription_id)
           FROM sw_sub_c2)
       IS DISTINCT FROM (SELECT array_agg('sub_sw_' || lpad(i::text, 3, '0') ORDER BY i)
                         FROM generate_series(51, 100) AS i) THEN
        RAISE EXCEPTION 'subscription claims 1-2 are not the 100 NULL leaders in id order';
    END IF;
    -- Returned user_id is the row's own user.
    IF EXISTS (SELECT 1 FROM sw_sub_c1 c JOIN subscriptions s USING (stripe_subscription_id)
               WHERE c.user_id <> s.user_id) THEN
        RAISE EXCEPTION 'subscription claim returned a wrong user_id';
    END IF;
    -- Coverage mix really was claimed: a paused NULL-coverage row and an
    -- active far-future row sit among the leaders.
    IF NOT EXISTS (SELECT 1 FROM sw_sub_c1 c JOIN subscriptions s USING (stripe_subscription_id)
                   WHERE s.status = 'paused' AND s.current_period_end IS NULL)
       OR NOT EXISTS (SELECT 1 FROM sw_sub_c1 c JOIN subscriptions s USING (stripe_subscription_id)
                      WHERE s.status = 'active' AND s.current_period_end > '2900-01-01') THEN
        RAISE EXCEPTION 'leader fixture lost paused/NULL or active/far-future coverage';
    END IF;

    -- Claim 3: the stale tail (due, sorted after NULLs) plus retried
    -- leaders, strictly from claim 1 (stamped earlier than claim 2).
    IF (SELECT count(*) FROM sw_sub_c3 WHERE stripe_subscription_id
            IN (SELECT 'sub_sw_' || lpad(i::text, 3, '0') FROM generate_series(101, 120) AS i)) <> 20 THEN
        RAISE EXCEPTION 'subscription claim 3 must include the 20 due tail rows';
    END IF;
    IF (SELECT count(*) FROM sw_sub_c3 JOIN sw_sub_c1 USING (stripe_subscription_id)) <> 30
       OR EXISTS (SELECT 1 FROM sw_sub_c3 JOIN sw_sub_c2 USING (stripe_subscription_id)) THEN
        RAISE EXCEPTION 'subscription claim 3 must retry 30 leaders from claim 1 only';
    END IF;

    -- Attempts advanced for every claimed row, with no state update.
    IF EXISTS (SELECT 1 FROM subscriptions s
               WHERE s.stripe_subscription_id IN (
                   SELECT stripe_subscription_id FROM sw_sub_c1
                   UNION SELECT stripe_subscription_id FROM sw_sub_c2
                   UNION SELECT stripe_subscription_id FROM sw_sub_c3)
                 AND s.reconcile_attempted_at IS NULL) THEN
        RAISE EXCEPTION 'a claimed subscription kept a NULL attempt stamp';
    END IF;
    IF EXISTS (SELECT 1 FROM sw_sub_c3 c
               JOIN sw_sub_pre3 p USING (stripe_subscription_id)
               JOIN subscriptions s USING (stripe_subscription_id)
               WHERE s.reconcile_attempted_at <= p.prev) THEN
        RAISE EXCEPTION 'a retried subscription attempt stamp did not advance';
    END IF;
    -- Unclaimed eligible rows (none left NULL) and ineligible rows stay put.
    IF EXISTS (SELECT 1 FROM subscriptions
               WHERE status IN ('incomplete', 'active', 'past_due', 'unpaid', 'paused')
                 AND reconcile_attempted_at IS NULL) THEN
        RAISE EXCEPTION 'an eligible subscription was never claimed';
    END IF;
    SELECT count(*) INTO n FROM subscriptions
    WHERE status IN ('canceled', 'incomplete_expired')
      AND reconcile_attempted_at IS NOT NULL;
    IF n <> 0 THEN
        RAISE EXCEPTION 'terminal subscriptions were stamped';
    END IF;
    IF (SELECT count(*) FROM subscriptions WHERE status IN ('canceled', 'incomplete_expired')) <> 5 THEN
        RAISE EXCEPTION 'terminal subscriptions changed';
    END IF;

    -- Nothing but the attempt stamp changed on any row.
    IF EXISTS (
        SELECT 1 FROM sw_sub_before b
        FULL JOIN (SELECT id, to_jsonb(s) - 'reconcile_attempted_at' AS body
                   FROM subscriptions s) a USING (id)
        WHERE b.body IS DISTINCT FROM a.body) THEN
        RAISE EXCEPTION 'subscription sweep changed status/paid dates/plan/other columns';
    END IF;
END $$;

-- ------------------------------------------------------ checkout/op sweep
CREATE TEMP TABLE sw_op_c1 AS SELECT * FROM pg_temp.claim_checkouts(50);
CREATE TEMP TABLE sw_op_c2 AS SELECT * FROM pg_temp.claim_checkouts(50);
CREATE TEMP TABLE sw_op_pre3 AS
SELECT id::text AS id, reconcile_attempted_at AS prev FROM billing_operations;
CREATE TEMP TABLE sw_op_c3 AS SELECT * FROM pg_temp.claim_checkouts(50);

DO $$
BEGIN
    IF (SELECT count(*) FROM sw_op_c1) <> 50
       OR (SELECT count(*) FROM sw_op_c2) <> 50
       OR (SELECT count(*) FROM sw_op_c3) <> 50 THEN
        RAISE EXCEPTION 'each operation claim must return exactly the limit (50)';
    END IF;
    IF EXISTS (SELECT 1 FROM sw_op_c1 JOIN sw_op_c2 USING (id)) THEN
        RAISE EXCEPTION 'operation claims 1 and 2 overlap';
    END IF;
    IF (SELECT array_agg(id ORDER BY id) FROM sw_op_c1)
       IS DISTINCT FROM (SELECT array_agg(('00000000-0000-0000-0002-' || lpad(i::text, 12, '0')) ORDER BY i)
                         FROM generate_series(1, 50) AS i)
       OR (SELECT array_agg(id ORDER BY id) FROM sw_op_c2)
       IS DISTINCT FROM (SELECT array_agg(('00000000-0000-0000-0002-' || lpad(i::text, 12, '0')) ORDER BY i)
                         FROM generate_series(51, 100) AS i) THEN
        RAISE EXCEPTION 'operation claims 1-2 are not the 100 NULL leaders in id order';
    END IF;

    -- Sessionless, expired and missing-subscription operations are all swept.
    IF NOT EXISTS (SELECT 1 FROM sw_op_c1 c JOIN billing_operations o ON o.id::text = c.id
                   WHERE o.stripe_subscription_id IS NULL AND o.request_payload = '{}'::jsonb)
       OR NOT EXISTS (SELECT 1 FROM sw_op_c1 c JOIN billing_operations o ON o.id::text = c.id
                      WHERE o.pending_update_expires_at < now())
       OR NOT EXISTS (SELECT 1 FROM sw_op_c1 c JOIN billing_operations o ON o.id::text = c.id
                      WHERE o.stripe_subscription_id LIKE 'sub_sw_missing_%'
                        AND NOT EXISTS (SELECT 1 FROM subscriptions s
                                        WHERE s.stripe_subscription_id = o.stripe_subscription_id)) THEN
        RAISE EXCEPTION 'operation fixture lost sessionless/expired/missing coverage';
    END IF;

    -- Returned projection (including JSONB as text) matches the stored row,
    -- for every claim.
    IF EXISTS (
        SELECT 1 FROM (SELECT * FROM sw_op_c1 UNION ALL SELECT * FROM sw_op_c2
                       UNION ALL SELECT * FROM sw_op_c3) c
        JOIN billing_operations o ON o.id::text = c.id
        WHERE c.user_id <> o.user_id OR c.kind <> o.kind OR c.state <> o.state
           OR c.stripe_subscription_id IS DISTINCT FROM o.stripe_subscription_id
           OR c.request_payload::jsonb IS DISTINCT FROM o.request_payload
           OR c.request_payload IS DISTINCT FROM o.request_payload::text) THEN
        RAISE EXCEPTION 'claimed operation projection differs from the stored row';
    END IF;

    -- Claim 3: 20 due tail rows plus 30 retried leaders from claim 1 only.
    IF (SELECT count(*) FROM sw_op_c3 WHERE id IN (
            SELECT '00000000-0000-0000-0002-' || lpad(i::text, 12, '0')
            FROM generate_series(101, 120) AS i)) <> 20 THEN
        RAISE EXCEPTION 'operation claim 3 must include the 20 due tail rows';
    END IF;
    IF (SELECT count(*) FROM sw_op_c3 JOIN sw_op_c1 USING (id)) <> 30
       OR EXISTS (SELECT 1 FROM sw_op_c3 JOIN sw_op_c2 USING (id)) THEN
        RAISE EXCEPTION 'operation claim 3 must retry 30 leaders from claim 1 only';
    END IF;

    -- Attempts advanced; no open operation left NULL.
    IF EXISTS (SELECT 1 FROM sw_op_c3 c
               JOIN sw_op_pre3 p USING (id)
               JOIN billing_operations o ON o.id::text = c.id
               WHERE o.reconcile_attempted_at <= p.prev) THEN
        RAISE EXCEPTION 'a retried operation attempt stamp did not advance';
    END IF;
    IF EXISTS (SELECT 1 FROM billing_operations
               WHERE state IN ('pending', 'submitted', 'needs_reconcile')
                 AND reconcile_attempted_at IS NULL) THEN
        RAISE EXCEPTION 'an open operation was never claimed';
    END IF;

    -- done/superseded operations are excluded and untouched.
    IF EXISTS (SELECT 1 FROM billing_operations
               WHERE state IN ('done', 'superseded')
                 AND reconcile_attempted_at IS NOT NULL) THEN
        RAISE EXCEPTION 'resolved operations were stamped';
    END IF;
    IF (SELECT count(*) FROM billing_operations WHERE state IN ('done', 'superseded')) <> 5 THEN
        RAISE EXCEPTION 'resolved operations changed';
    END IF;

    -- Nothing but the attempt stamp changed on any operation.
    IF EXISTS (
        SELECT 1 FROM sw_op_before b
        FULL JOIN (SELECT id, to_jsonb(o) - 'reconcile_attempted_at' AS body
                   FROM billing_operations o) a USING (id)
        WHERE b.body IS DISTINCT FROM a.body) THEN
        RAISE EXCEPTION 'operation sweep changed state/payload/other columns';
    END IF;

    -- Sweeping never grants usage.
    IF (SELECT count(*) FROM stream_play_ledger) <> (SELECT n FROM sw_usage_before) THEN
        RAISE EXCEPTION 'sweeps must not touch usage state';
    END IF;
END $$;

ROLLBACK;
