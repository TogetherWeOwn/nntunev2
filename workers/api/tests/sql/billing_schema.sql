-- Regression test for migration 0021. Run after
-- ALL migrations are applied (see ../run_migration_tests.sh). Everything
-- happens inside one transaction that is rolled back, so it leaves no rows;
-- any broken expectation RAISEs and (with ON_ERROR_STOP) fails the run.
--
-- Scope: the ADR 0002 billing shapes exist with their single-flight / slot
-- partial uniques, the plans seed matches the ADR 0004 decision table, and
-- migration 0021 did not collide with the slice-1 precursor
-- `stream_play_ledger` (migration 0020): both tables exist with distinct
-- shapes (monthly `period_month` vs daily `period DATE`).

BEGIN;

DO $$
BEGIN
    -- All eight billing tables exist.
    IF (SELECT count(*) FROM (
        VALUES ('plans'), ('plan_prices'), ('billing_accounts'),
               ('billing_operations'), ('billing_webhook_events'),
               ('subscriptions'), ('usage_ledger'), ('ingest_jobs')
    ) AS want(name)
    WHERE to_regclass('public.' || want.name) IS NULL) <> 0 THEN
        RAISE EXCEPTION 'a 0021 billing table is missing';
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
    -- And the daily ledger is the distinct ADR shape, not a second monthly
    -- table wearing its name.
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = 'usage_ledger' AND column_name = 'period_month'
    ) THEN
        RAISE EXCEPTION 'usage_ledger must not carry the precursor month shape';
    END IF;
    IF (SELECT data_type FROM information_schema.columns
        WHERE table_name = 'usage_ledger' AND column_name = 'period') <> 'date' THEN
        RAISE EXCEPTION 'usage_ledger.period must be DATE';
    END IF;

    -- Plans seed: exactly the four ADR 0004 tiers with their caps, prices
    -- ($ to cents) and cache budgets (GiB to bytes). The Rust
    -- seed_rows_match_adr_0004 test parses the ADR table for the same
    -- values; this pins them in the applied database too.
    IF (SELECT count(*) FROM plans) <> 4 THEN
        RAISE EXCEPTION 'plans must seed exactly four tiers';
    END IF;
    IF (SELECT array_agg(
            id || ':' || price_cents || ':' || stream_hours_month || ':' ||
            max_concurrent_streams || ':' || max_library_tracks || ':' ||
            ingest_jobs_day || ':' || cache_priority || ':' ||
            cache_bytes_budget || ':' || cache_retention_days
            ORDER BY id)
        FROM plans)
       IS DISTINCT FROM ARRAY[
            'free:0:10:1:1000:5:low:2147483648:7',
            'plus:800:100:2:25000:25:standard:21474836480:30',
            'pro-20x:10000:2000:8:500000:400:highest:429496729600:90',
            'pro-5x:3000:500:4:100000:100:high:107374182400:90'
        ] THEN
        RAISE EXCEPTION 'plans seed drifted from the ADR 0004 decision table';
    END IF;
    -- Seed replay safety: the gate reapplies every migration, so reseeding
    -- must be a no-op, not a 23505.
    INSERT INTO plans (id, price_cents, stream_hours_month,
        max_concurrent_streams, max_library_tracks, ingest_jobs_day,
        cache_priority, cache_bytes_budget, cache_retention_days)
    VALUES ('free', 0, 10, 1, 1000, 5, 'low', 2147483648, 7)
    ON CONFLICT (id) DO NOTHING;

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
    VALUES ('user-bil-1', 'trial_create', 'pending', 'req-1', '{}');
    BEGIN
        INSERT INTO billing_operations
            (user_id, kind, state, request_key, request_payload)
        VALUES ('user-bil-1', 'trial_create', 'submitted', 'req-2', '{}');
        RAISE EXCEPTION 'single-flight partial unique accepted a second row';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    INSERT INTO billing_operations
        (user_id, kind, state, request_key, request_payload)
    VALUES ('user-bil-2', 'trial_create', 'needs_reconcile', 'req-3', '{}');
    -- needs_reconcile counts as unresolved: a fresh create beside it must be
    -- refused (fresh request key, so only the single-flight predicate can
    -- fire — dropping the state from it must fail the gate).
    BEGIN
        INSERT INTO billing_operations
            (user_id, kind, state, request_key, request_payload)
        VALUES ('user-bil-2', 'paid_convert', 'pending', 'req-3-conflict', '{}');
        RAISE EXCEPTION 'single flight accepted a row beside needs_reconcile';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    UPDATE billing_operations SET state = 'done' WHERE request_key = 'req-1';
    INSERT INTO billing_operations
        (user_id, kind, state, request_key, request_payload)
    VALUES ('user-bil-1', 'paid_convert', 'submitted', 'req-4', '{}');
    -- Terminal state, so the duplicate key below can only violate the
    -- request_key UNIQUE — user-bil-2 still holds the unresolved req-3 row,
    -- and a pending/submitted/needs_reconcile state would trip single flight
    -- too (same SQLSTATE, unisolated assertion).
    BEGIN
        INSERT INTO billing_operations
            (user_id, kind, state, request_key, request_payload)
        VALUES ('user-bil-2', 'paid_convert', 'done', 'req-3', '{}');
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
                'plus', 'trialing');
        RAISE EXCEPTION 'NULL stripe_subscription_id was accepted';
    EXCEPTION WHEN not_null_violation THEN
        NULL;
    END;
    INSERT INTO subscriptions
        (user_id, stripe_subscription_id, stripe_customer_id,
         stripe_subscription_item_id, stripe_price_id, plan_id, status,
         trial_end)
    VALUES ('user-bil-1', 'sub_live_1', 'cus_1', 'si_1', 'price_sel_plus',
            'free', 'trialing', now() + interval '13 days');
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
    -- Every remaining live-slot status conflicts with the held trialing row:
    -- dropping any one of them from the predicate must fail the gate.
    BEGIN
        INSERT INTO subscriptions
            (user_id, stripe_subscription_id, stripe_customer_id,
             stripe_subscription_item_id, stripe_price_id, plan_id, status)
        VALUES ('user-bil-1', 'sub_live_incomplete', 'cus_1', 'si_2',
                'price_sel_plus', 'plus', 'incomplete');
        RAISE EXCEPTION 'live slot accepted incomplete beside trialing';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO subscriptions
            (user_id, stripe_subscription_id, stripe_customer_id,
             stripe_subscription_item_id, stripe_price_id, plan_id, status)
        VALUES ('user-bil-1', 'sub_live_past_due', 'cus_1', 'si_2',
                'price_sel_plus', 'plus', 'past_due');
        RAISE EXCEPTION 'live slot accepted past_due beside trialing';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO subscriptions
            (user_id, stripe_subscription_id, stripe_customer_id,
             stripe_subscription_item_id, stripe_price_id, plan_id, status)
        VALUES ('user-bil-1', 'sub_live_unpaid', 'cus_1', 'si_2',
                'price_sel_plus', 'plus', 'unpaid');
        RAISE EXCEPTION 'live slot accepted unpaid beside trialing';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO subscriptions
            (user_id, stripe_subscription_id, stripe_customer_id,
             stripe_subscription_item_id, stripe_price_id, plan_id, status)
        VALUES ('user-bil-1', 'sub_live_paused', 'cus_1', 'si_2',
                'price_sel_plus', 'plus', 'paused');
        RAISE EXCEPTION 'live slot accepted paused beside trialing';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    -- Terminal rows do not reserve the slot; unknown provider states are
    -- stored verbatim (fail closed at read, never rejected at write).
    INSERT INTO subscriptions
        (user_id, stripe_subscription_id, stripe_customer_id,
         stripe_subscription_item_id, stripe_price_id, plan_id, status)
    VALUES ('user-bil-1', 'sub_old_1', 'cus_1', 'si_0', 'price_old_plus',
            'plus', 'canceled'),
           ('user-bil-1', 'sub_old_2', 'cus_1', 'si_7', 'price_old_plus',
            'plus', 'incomplete_expired'),
           ('user-bil-1', 'sub_odd_1', 'cus_1', 'si_9', 'price_sel_plus',
            'plus', 'weird_remote_status');
    IF (SELECT count(*) FROM subscriptions WHERE user_id = 'user-bil-1') <> 4 THEN
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

    -- usage_ledger: one row per (user, day); stream/ingest writers keep
    -- their own counters with additive upserts that never lose each other.
    INSERT INTO usage_ledger (user_id, period, stream_seconds)
    VALUES ('user-bil-1', '2026-10-18', 3600);
    INSERT INTO usage_ledger (user_id, period, ingest_jobs)
    VALUES ('user-bil-1', '2026-10-18', 2)
    ON CONFLICT (user_id, period) DO UPDATE
        SET ingest_jobs = usage_ledger.ingest_jobs + EXCLUDED.ingest_jobs;
    IF (SELECT stream_seconds || ':' || ingest_jobs FROM usage_ledger
        WHERE user_id = 'user-bil-1' AND period = '2026-10-18')
       <> '3600:2' THEN
        RAISE EXCEPTION 'additive ledger upsert lost a counter';
    END IF;
    BEGIN
        INSERT INTO usage_ledger (user_id, period)
        VALUES ('user-bil-1', '2026-10-18');
        RAISE EXCEPTION 'UNIQUE accepted a duplicate (user_id, period)';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO usage_ledger (user_id, period, stream_seconds)
        VALUES ('user-bil-1', '2026-10-19', -1);
        RAISE EXCEPTION 'CHECK accepted negative stream_seconds';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO usage_ledger (user_id, period, ingest_jobs)
        VALUES ('user-bil-1', '2026-10-19', -1);
        RAISE EXCEPTION 'CHECK accepted negative ingest_jobs';
    EXCEPTION WHEN check_violation THEN
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
END
$$;

ROLLBACK;
