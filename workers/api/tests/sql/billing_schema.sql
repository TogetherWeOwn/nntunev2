-- Regression test for migration 0021 as superseded by the 0026 plan
-- reseed (effective schema). Run after ALL migrations are applied (see
-- ../run_migration_tests.sh). Everything happens inside one transaction
-- that is rolled back, so it leaves no rows; any broken expectation RAISEs
-- and (with ON_ERROR_STOP) fails the run.
--
-- Scope: the ADR 0002 billing shapes exist with their single-flight / slot
-- partial uniques, the plans seed matches the ADR 0004 decision table,
-- the retired daily `usage_ledger` is gone, and migration 0021 did not
-- collide with the slice-1 precursor `stream_play_ledger` (migration
-- 0020), which is now the monthly stream-usage authority.

BEGIN;

DO $$
BEGIN
    -- All seven billing tables exist.
    IF (SELECT count(*) FROM (
        VALUES ('plans'), ('plan_prices'), ('billing_accounts'),
               ('billing_operations'), ('billing_webhook_events'),
               ('subscriptions'), ('ingest_jobs')
    ) AS want(name)
    WHERE to_regclass('public.' || want.name) IS NULL) <> 0 THEN
        RAISE EXCEPTION 'a 0021 billing table is missing';
    END IF;
    -- The daily fold table was dropped by 0026: monthly stream usage
    -- derives from `stream_play_ledger`, daily ingest from `ingest_jobs`.
    IF to_regclass('public.usage_ledger') IS NOT NULL THEN
        RAISE EXCEPTION 'usage_ledger must be dropped by 0026';
    END IF;

    -- The precursor survives with its own shape (no silent collision).
    IF to_regclass('public.stream_play_ledger') IS NULL THEN
        RAISE EXCEPTION 'stream_play_ledger missing after 0021';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = 'stream_play_ledger' AND column_name = 'period_month'
    ) THEN
        RAISE EXCEPTION 'stream_play_ledger lost its period_month shape';
    END IF;
    -- Plans seed: exactly the four ADR 0004 rows (reserved free + three
    -- paid) with price ($ to cents), monthly stream hours and concurrent
    -- streams — the only plan dimensions left. The Rust
    -- seed_rows_match_adr_0004 test parses the ADR table for the same
    -- values; this pins them in the applied database too.
    IF (SELECT count(*) FROM plans) <> 4 THEN
        RAISE EXCEPTION 'plans must seed exactly four tiers';
    END IF;
    IF (SELECT array_agg(
            id || ':' || price_cents || ':' || stream_hours_month || ':' ||
            max_concurrent_streams
            ORDER BY id)
        FROM plans)
       IS DISTINCT FROM ARRAY[
            'free:0:0:1',
            'max:2500:750:1',
            'plus:800:100:1',
            'pro:1500:300:1'
        ] THEN
        RAISE EXCEPTION 'plans seed drifted from the ADR 0004 decision table';
    END IF;
    -- Seed replay safety: 0026's upsert re-asserts the values on replay
    -- instead of raising 23505.
    INSERT INTO plans (id, price_cents, stream_hours_month, max_concurrent_streams)
    VALUES ('free', 0, 0, 1)
    ON CONFLICT (id) DO UPDATE SET
        price_cents = EXCLUDED.price_cents,
        stream_hours_month = EXCLUDED.stream_hours_month,
        max_concurrent_streams = EXCLUDED.max_concurrent_streams;

    INSERT INTO users (id, email, created_at)
    VALUES ('user-bil-1', 'bil-1@example.com', 1700000000),
           ('user-bil-2', 'bil-2@example.com', 1700000000);

    -- plan_prices: at most one selectable price per plan; history stays as
    -- nonselectable rows.
    INSERT INTO plan_prices
        (stripe_price_id, plan_id, currency, price_cents,
         billing_interval, livemode, selectable)
    VALUES ('price_sel_plus', 'plus', 'usd', 800, 'month', FALSE, TRUE),
           ('price_old_plus', 'plus', 'usd', 800, 'month', FALSE, FALSE);
    BEGIN
        INSERT INTO plan_prices
            (stripe_price_id, plan_id, currency, price_cents,
             billing_interval, livemode, selectable)
        VALUES ('price_sel_plus_2', 'plus', 'usd', 800, 'month', FALSE, TRUE);
        RAISE EXCEPTION 'selectable partial unique accepted a second price';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    -- Unknown plan ids cannot be mapped (relational mapping authority).
    BEGIN
        INSERT INTO plan_prices
            (stripe_price_id, plan_id, currency, price_cents,
             billing_interval, livemode, selectable)
        VALUES ('price_nope', 'enterprise', 'usd', 1, 'month', FALSE, FALSE);
        RAISE EXCEPTION 'FK accepted a price for an unknown plan';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;
    END;

    -- billing_accounts: one row per user; customer ids unique when present,
    -- NULL when Stripe has not been reached yet.
    INSERT INTO billing_accounts (user_id, stripe_customer_id)
    VALUES ('user-bil-1', NULL), ('user-bil-2', NULL);
    BEGIN
        INSERT INTO billing_accounts (user_id) VALUES ('user-bil-1');
        RAISE EXCEPTION 'billing_accounts accepted a second row for a user';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO billing_accounts (user_id) VALUES ('user-missing');
        RAISE EXCEPTION 'FK accepted a billing account for a missing user';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;
    END;
    UPDATE billing_accounts SET stripe_customer_id = 'cus_1'
    WHERE user_id = 'user-bil-1';
    BEGIN
        UPDATE billing_accounts SET stripe_customer_id = 'cus_1'
        WHERE user_id = 'user-bil-2';
        RAISE EXCEPTION 'UNIQUE accepted a duplicate stripe_customer_id';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;

    -- billing_operations single flight: one unresolved row per user; a
    -- resolved row beside it is fine; request keys stay globally unique.
    INSERT INTO billing_operations
        (user_id, kind, state, request_key, request_payload)
    VALUES ('user-bil-1', 'checkout_create', 'pending', 'req-1', '{}');
    BEGIN
        INSERT INTO billing_operations
            (user_id, kind, state, request_key, request_payload)
        VALUES ('user-bil-1', 'checkout_create', 'submitted', 'req-2', '{}');
        RAISE EXCEPTION 'single-flight partial unique accepted a second row';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    INSERT INTO billing_operations
        (user_id, kind, state, request_key, request_payload)
    VALUES ('user-bil-2', 'checkout_create', 'needs_reconcile', 'req-3', '{}');
    -- needs_reconcile counts as unresolved: a fresh create beside it must be
    -- refused (fresh request key, so only the single-flight predicate can
    -- fire — dropping the state from it must fail the gate).
    BEGIN
        INSERT INTO billing_operations
            (user_id, kind, state, request_key, request_payload)
        VALUES ('user-bil-2', 'plan_change', 'pending', 'req-3-conflict', '{}');
        RAISE EXCEPTION 'single flight accepted a row beside needs_reconcile';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    UPDATE billing_operations SET state = 'done' WHERE request_key = 'req-1';
    INSERT INTO billing_operations
        (user_id, kind, state, request_key, request_payload)
    VALUES ('user-bil-1', 'plan_change', 'submitted', 'req-4', '{}');
    -- Terminal state, so the duplicate key below can only violate the
    -- request_key UNIQUE — user-bil-2 still holds the unresolved req-3 row,
    -- and a pending/submitted/needs_reconcile state would trip single flight
    -- too (same SQLSTATE, unisolated assertion).
    BEGIN
        INSERT INTO billing_operations
            (user_id, kind, state, request_key, request_payload)
        VALUES ('user-bil-2', 'plan_change', 'done', 'req-3', '{}');
        RAISE EXCEPTION 'UNIQUE accepted a duplicate request_key';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;

    -- billing_webhook_events: duplicate delivery converges on the PK.
    INSERT INTO billing_webhook_events (stripe_event_id)
    VALUES ('evt_1');
    BEGIN
        INSERT INTO billing_webhook_events (stripe_event_id)
        VALUES ('evt_1');
        RAISE EXCEPTION 'webhook event PK accepted a duplicate delivery';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;

    -- subscriptions: the Stripe id is NOT NULL and globally unique; the
    -- live slot holds one nonterminal row per user while terminal rows sit
    -- outside it.
    BEGIN
        INSERT INTO subscriptions
            (user_id, stripe_subscription_id, stripe_customer_id,
             stripe_subscription_item_id, stripe_price_id, plan_id, status)
        VALUES ('user-bil-1', NULL, 'cus_1', 'si_1', 'price_sel_plus',
                'plus', 'active');
        RAISE EXCEPTION 'NULL stripe_subscription_id was accepted';
    EXCEPTION WHEN not_null_violation THEN
        NULL;
    END;
    INSERT INTO subscriptions
        (user_id, stripe_subscription_id, stripe_customer_id,
         stripe_subscription_item_id, stripe_price_id, plan_id, status)
    VALUES ('user-bil-1', 'sub_live_1', 'cus_1', 'si_1', 'price_sel_plus',
            'plus', 'active');
    BEGIN
        INSERT INTO subscriptions
            (user_id, stripe_subscription_id, stripe_customer_id,
             stripe_subscription_item_id, stripe_price_id, plan_id, status)
        VALUES ('user-bil-1', 'sub_live_2', 'cus_1', 'si_2',
                'price_sel_plus', 'plus', 'active');
        RAISE EXCEPTION 'live-slot partial unique accepted a second row';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    -- Every remaining live-slot status conflicts with the held active row:
    -- dropping any one of them from the predicate must fail the gate.
    BEGIN
        INSERT INTO subscriptions
            (user_id, stripe_subscription_id, stripe_customer_id,
             stripe_subscription_item_id, stripe_price_id, plan_id, status)
        VALUES ('user-bil-1', 'sub_live_incomplete', 'cus_1', 'si_2',
                'price_sel_plus', 'plus', 'incomplete');
        RAISE EXCEPTION 'live slot accepted incomplete beside active';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO subscriptions
            (user_id, stripe_subscription_id, stripe_customer_id,
             stripe_subscription_item_id, stripe_price_id, plan_id, status)
        VALUES ('user-bil-1', 'sub_live_past_due', 'cus_1', 'si_2',
                'price_sel_plus', 'plus', 'past_due');
        RAISE EXCEPTION 'live slot accepted past_due beside active';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO subscriptions
            (user_id, stripe_subscription_id, stripe_customer_id,
             stripe_subscription_item_id, stripe_price_id, plan_id, status)
        VALUES ('user-bil-1', 'sub_live_unpaid', 'cus_1', 'si_2',
                'price_sel_plus', 'plus', 'unpaid');
        RAISE EXCEPTION 'live slot accepted unpaid beside active';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO subscriptions
            (user_id, stripe_subscription_id, stripe_customer_id,
             stripe_subscription_item_id, stripe_price_id, plan_id, status)
        VALUES ('user-bil-1', 'sub_live_paused', 'cus_1', 'si_2',
                'price_sel_plus', 'plus', 'paused');
        RAISE EXCEPTION 'live slot accepted paused beside active';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    -- Terminal rows do not reserve the slot; unknown provider states are
    -- stored verbatim (fail closed at read, never rejected at write) —
    -- including a Stripe-reported `trialing`, which no flow can produce
    -- any more and which must not reserve the slot.
    INSERT INTO subscriptions
        (user_id, stripe_subscription_id, stripe_customer_id,
         stripe_subscription_item_id, stripe_price_id, plan_id, status)
    VALUES ('user-bil-1', 'sub_old_1', 'cus_1', 'si_0', 'price_old_plus',
            'plus', 'canceled'),
           ('user-bil-1', 'sub_old_2', 'cus_1', 'si_7', 'price_old_plus',
            'plus', 'incomplete_expired'),
           ('user-bil-1', 'sub_odd_1', 'cus_1', 'si_9', 'price_sel_plus',
            'plus', 'weird_remote_status'),
           ('user-bil-1', 'sub_odd_2', 'cus_1', 'si_10', 'price_sel_plus',
            'plus', 'trialing');
    IF (SELECT count(*) FROM subscriptions WHERE user_id = 'user-bil-1') <> 5 THEN
        RAISE EXCEPTION 'terminal/unknown rows must coexist with the live row';
    END IF;
    BEGIN
        INSERT INTO subscriptions
            (user_id, stripe_subscription_id, stripe_customer_id,
             stripe_subscription_item_id, stripe_price_id, plan_id, status)
        VALUES ('user-bil-2', 'sub_live_1', 'cus_1', 'si_3',
                'price_sel_plus', 'plus', 'active');
        RAISE EXCEPTION 'UNIQUE accepted a duplicate stripe_subscription_id';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;

    -- ingest_jobs: idempotent accept on (user_id, idempotency_key); unknown
    -- statuses cannot be stored.
    INSERT INTO ingest_jobs (user_id, idempotency_key, status)
    VALUES ('user-bil-1', 'idem-1', 'queued');
    BEGIN
        INSERT INTO ingest_jobs (user_id, idempotency_key, status)
        VALUES ('user-bil-1', 'idem-1', 'queued');
        RAISE EXCEPTION 'UNIQUE accepted a duplicate (user_id, idempotency_key)';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO ingest_jobs (user_id, idempotency_key, status)
        VALUES ('user-bil-1', 'idem-2', 'vibing');
        RAISE EXCEPTION 'CHECK accepted an unknown ingest status';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- PgStore daily ingest count + QA seed (worker_entry.rs): the count
    -- derives from accepted rows on the BIGINT epoch-day boundary
    -- (accepted_at / 86400, the 0022 converged shape), and the seed's
    -- deterministic QA keys make a reseed a no-op that never lowers the
    -- count. Fixed far-future epoch day (1893456000 = 2030-01-01) on
    -- user-bil-2, who has no other ingest rows.
    INSERT INTO ingest_jobs (user_id, idempotency_key, accepted_at, status)
    SELECT 'user-bil-2', 'qa-seed:' || g, 1893456000, 'done'
    FROM generate_series(1, 5::bigint) AS g
    ON CONFLICT (user_id, idempotency_key) DO NOTHING;
    IF (SELECT COUNT(*)::BIGINT FROM ingest_jobs
        WHERE user_id = 'user-bil-2' AND accepted_at / 86400 = 1893456000 / 86400) <> 5 THEN
        RAISE EXCEPTION 'daily ingest count must see the five seeded rows';
    END IF;
    INSERT INTO ingest_jobs (user_id, idempotency_key, accepted_at, status)
    SELECT 'user-bil-2', 'qa-seed:' || g, 1893456000, 'done'
    FROM generate_series(1, 5::bigint) AS g
    ON CONFLICT (user_id, idempotency_key) DO NOTHING;
    IF (SELECT COUNT(*)::BIGINT FROM ingest_jobs
        WHERE user_id = 'user-bil-2' AND accepted_at / 86400 = 1893456000 / 86400) <> 5 THEN
        RAISE EXCEPTION 'QA reseed must be a no-op, never a double count';
    END IF;
    IF (SELECT COUNT(*)::BIGINT FROM ingest_jobs
        WHERE user_id = 'user-bil-2'
          AND accepted_at / 86400 = (1893456000 + 86400) / 86400) <> 0 THEN
        RAISE EXCEPTION 'epoch-day boundary leaked into the next day';
    END IF;
    -- A job accepted at 23:59:59 belongs to that UTC day; one accepted at
    -- the next midnight does not (rows carry the boundary, not just the
    -- query instant).
    INSERT INTO ingest_jobs (user_id, idempotency_key, accepted_at, status)
    VALUES ('user-bil-2', 'qa-seed:late', 1893456000 + 86399, 'done'),
           ('user-bil-2', 'qa-seed:nextday', 1893456000 + 86400, 'done');
    IF (SELECT COUNT(*)::BIGINT FROM ingest_jobs
        WHERE user_id = 'user-bil-2'
          AND accepted_at / 86400 = (1893456000 + 86399) / 86400) <> 6 THEN
        RAISE EXCEPTION 'job accepted at 23:59:59 must count in its UTC day';
    END IF;
    IF (SELECT COUNT(*)::BIGINT FROM ingest_jobs
        WHERE user_id = 'user-bil-2'
          AND accepted_at / 86400 = (1893456000 + 86400) / 86400) <> 1 THEN
        RAISE EXCEPTION 'job accepted at next midnight must count only in the next day';
    END IF;
END
$$;

ROLLBACK;
