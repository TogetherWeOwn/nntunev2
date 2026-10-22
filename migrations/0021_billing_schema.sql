-- NNTune v2 migration 0021: billing schema + effective-plan read path (Slice-2).
--
-- PlanetScale Postgres via Hyperdrive. All access uses the parameterized
-- statements in workers/api/src/billing.rs (never string-interpolated); the
-- effective-plan predicate there resolves (plan_id, entitled) from the
-- subscriptions + billing_accounts rows per ADR 0002 rule 4 / ADR 0005.
--
-- Scope: schema + read path only. No Stripe API call, webhook worker,
-- checkout/portal flow, price activation, or spend happens here; the
-- plan_prices mapping rows are seeded per environment by a later gated
-- slice (staging migration apply itself is a gated host action).
--
-- Table identity: these are the accepted ADR 0002 billing shapes. They do
-- NOT collide with the slice-1 precursor `stream_play_ledger`
-- (migration 0020: per-play windows, `play:` keys, monthly `period_month`
-- folds). In particular this migration's daily `usage_ledger`
-- (`period DATE`, `UNIQUE (user_id, period)`, flows only) has a distinct
-- name AND a distinct shape from that precursor, so neither CREATE
-- silently no-ops on the other.
--
-- Replay safety: every CREATE is IF NOT EXISTS (the migration gate
-- reapplies each file), the plans seed is ON CONFLICT DO NOTHING, and the
-- partial unique indexes are IF NOT EXISTS. New ids are UUID
-- (gen_random_uuid, the migration 0006 precedent), new timestamps are
-- timestamptz, and new user_id FKs are TEXT matching users.id with
-- ON DELETE CASCADE (the migration 0020 convention; account erasure still
-- deletes billing rows explicitly through the ADR 0002 rule-3 gate before
-- the user row goes).

CREATE TABLE IF NOT EXISTS plans (
    id TEXT PRIMARY KEY,
    price_cents INTEGER NOT NULL CHECK (price_cents >= 0),
    stream_hours_month INTEGER NOT NULL CHECK (stream_hours_month >= 0),
    max_concurrent_streams INTEGER NOT NULL CHECK (max_concurrent_streams >= 1),
    max_library_tracks BIGINT NOT NULL CHECK (max_library_tracks >= 0),
    ingest_jobs_day INTEGER NOT NULL CHECK (ingest_jobs_day >= 0),
    cache_priority TEXT NOT NULL,
    cache_bytes_budget BIGINT NOT NULL CHECK (cache_bytes_budget >= 0),
    cache_retention_days INTEGER NOT NULL CHECK (cache_retention_days >= 0)
);

-- Seed rows are the ADR 0004 decision table verbatim (price $ to cents,
-- cache GiB to bytes at 1,073,741,824). workers/api/src/billing.rs parses
-- that table with include_str! and asserts each of these tuples, so editing
-- the ADR without reseeding (or vice versa) fails the suite.
INSERT INTO plans (id, price_cents, stream_hours_month, max_concurrent_streams, max_library_tracks, ingest_jobs_day, cache_priority, cache_bytes_budget, cache_retention_days) VALUES
    ('free', 0, 10, 1, 1000, 5, 'low', 2147483648, 7),
    ('plus', 800, 100, 2, 25000, 25, 'standard', 21474836480, 30),
    ('pro-5x', 3000, 500, 4, 100000, 100, 'high', 107374182400, 90),
    ('pro-20x', 10000, 2000, 8, 500000, 400, 'highest', 429496729600, 90)
ON CONFLICT (id) DO NOTHING;

CREATE TABLE IF NOT EXISTS plan_prices (
    stripe_price_id TEXT PRIMARY KEY,
    plan_id TEXT NOT NULL REFERENCES plans (id),
    currency TEXT NOT NULL,
    price_cents INTEGER NOT NULL CHECK (price_cents >= 0),
    billing_interval TEXT NOT NULL,
    livemode BOOLEAN NOT NULL,
    selectable BOOLEAN NOT NULL DEFAULT FALSE
);
-- At most one selectable price per plan (ADR 0002); historical mappings stay
-- as nonselectable rows so old invoices still reconcile.
CREATE UNIQUE INDEX IF NOT EXISTS uq_plan_prices_selectable_plan
    ON plan_prices (plan_id) WHERE selectable;

CREATE TABLE IF NOT EXISTS billing_accounts (
    user_id TEXT PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    stripe_customer_id TEXT UNIQUE NULL,
    trial_started_at TIMESTAMPTZ NULL
);

CREATE TABLE IF NOT EXISTS billing_operations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    state TEXT NOT NULL,
    request_key TEXT UNIQUE NOT NULL,
    request_payload JSONB NOT NULL,
    stripe_subscription_id TEXT NULL,
    stripe_invoice_id TEXT NULL,
    pending_update_expires_at TIMESTAMPTZ NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- Single flight: at most one unresolved billing operation per user
-- (ADR 0005 creation contract).
CREATE UNIQUE INDEX IF NOT EXISTS uq_billing_operations_single_flight
    ON billing_operations (user_id)
    WHERE state IN ('pending', 'submitted', 'needs_reconcile');

CREATE TABLE IF NOT EXISTS billing_webhook_events (
    stripe_event_id TEXT PRIMARY KEY,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    processed_at TIMESTAMPTZ NULL
);

CREATE TABLE IF NOT EXISTS subscriptions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    stripe_subscription_id TEXT UNIQUE NOT NULL,
    stripe_customer_id TEXT NOT NULL,
    stripe_subscription_item_id TEXT NOT NULL,
    stripe_price_id TEXT NOT NULL,
    plan_id TEXT NOT NULL REFERENCES plans (id),
    status TEXT NOT NULL,
    current_period_end TIMESTAMPTZ NULL,
    cancel_at_period_end BOOLEAN NOT NULL DEFAULT FALSE,
    trial_end TIMESTAMPTZ NULL,
    paid_through_at TIMESTAMPTZ NULL,
    paid_invoice_id TEXT NULL,
    grace_until TIMESTAMPTZ NULL
);
-- The live-subscription slot (ADR 0002 rule 4 / ADR 0005): the six
-- nonterminal statuses reserve it; only confirmed `canceled` and
-- `incomplete_expired` rows sit outside. Status is stored verbatim with no
-- CHECK so unknown provider states remain recorded and fail closed at read
-- time instead of being rejected at write time.
CREATE UNIQUE INDEX IF NOT EXISTS uq_subscriptions_live_slot
    ON subscriptions (user_id)
    WHERE status IN ('incomplete', 'trialing', 'active', 'past_due', 'unpaid', 'paused');

-- Daily flow ledger (ADR 0002 rule 5): stream seconds plus accepted ingest
-- job counts per UTC day. Flows only — no cache gauge. Stream and ingest
-- writers insert or increment only their own counter via additive
-- ON CONFLICT (user_id, period) DO UPDATE.
CREATE TABLE IF NOT EXISTS usage_ledger (
    id BIGSERIAL PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    period DATE NOT NULL,
    stream_seconds BIGINT NOT NULL DEFAULT 0 CHECK (stream_seconds >= 0),
    ingest_jobs BIGINT NOT NULL DEFAULT 0 CHECK (ingest_jobs >= 0),
    UNIQUE (user_id, period)
);

CREATE TABLE IF NOT EXISTS ingest_jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    idempotency_key TEXT NOT NULL,
    accepted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    status TEXT NOT NULL CHECK (status IN ('queued', 'running', 'done', 'failed')),
    UNIQUE (user_id, idempotency_key)
);
