-- NNTune v2 migration 0026: plan reseed + trial/usage_ledger retirement
-- (ADR 0004/0005/0002 revision of 2026-11-03, TWO-1629/TWO-1669).
--
-- Forward-only correction of migration 0021's seed and shapes; 0021 itself
-- is never edited. Effective state after this file:
-- - `plans` holds exactly the ADR 0004 rows (reserved non-selectable
--   `free` at $0/0 h, `plus` $8/100 h, `pro` $15/300 h, `max` $25/750 h,
--   1 concurrent stream each) and only the price / stream-hours /
--   concurrency columns: the ingest/library/cache plan dimensions are gone.
-- - The retired `pro-5x` / `pro-20x` rows are deleted, guarded by a loud
--   assertion that nothing references them (pre-launch there are no
--   customers and `plan_prices` is empty; if that is ever untrue this
--   migration must fail rather than strand a paying row).
-- - The daily `usage_ledger` fold table is dropped: monthly usage is the
--   direct SUM over `stream_play_ledger` (0020), and the ingest daily
--   count derives from accepted `ingest_jobs` rows.
-- - Trial columns are dropped (`billing_accounts.trial_started_at`,
--   `subscriptions.trial_end`) and the live-slot partial unique is
--   recreated without 'trialing': status stays free-form TEXT, so a
--   Stripe-reported `trialing` is an unknown status that fails closed at
--   read time and does not reserve the slot.
--
-- Replay safety: this file is idempotent (guard re-checks, DROP ... IF
-- EXISTS, upsert). Migration 0021 is NOT replay-safe after this file has
-- dropped its columns (its seed INSERT names them), so the migration
-- test harness excludes 0021 from its replay pass (see
-- workers/api/tests/run_migration_tests.sh), the same treatment as 0006.

-- Guarded assertion: retired plans must be unreferenced. Fail loudly;
-- never delete a plan row out from under a price mapping or subscription.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM plan_prices WHERE plan_id IN ('pro-5x', 'pro-20x'))
        OR EXISTS (SELECT 1 FROM subscriptions WHERE plan_id IN ('pro-5x', 'pro-20x')) THEN
        RAISE EXCEPTION 'retired plans pro-5x/pro-20x are still referenced; refusing plan reseed';
    END IF;
END
$$;

-- Drop the dropped-dimension columns first: they are NOT NULL without
-- defaults, so the reseed INSERT below could not create the new
-- `pro`/`max` rows while they exist.
ALTER TABLE plans DROP COLUMN IF EXISTS max_library_tracks;
ALTER TABLE plans DROP COLUMN IF EXISTS ingest_jobs_day;
ALTER TABLE plans DROP COLUMN IF EXISTS cache_priority;
ALTER TABLE plans DROP COLUMN IF EXISTS cache_bytes_budget;
ALTER TABLE plans DROP COLUMN IF EXISTS cache_retention_days;

-- Reseed to the ADR 0004 decision table verbatim (price $ to cents).
-- workers/api/src/billing.rs parses that table with include_str! and
-- asserts each of these tuples, so editing the ADR without reseeding
-- (or vice versa) fails the suite. The upsert also corrects the 0021
-- `free` (10 h) and `plus` (2 concurrent) rows in place.
INSERT INTO plans (id, price_cents, stream_hours_month, max_concurrent_streams) VALUES
    ('free', 0, 0, 1),
    ('plus', 800, 100, 1),
    ('pro', 1500, 300, 1),
    ('max', 2500, 750, 1)
ON CONFLICT (id) DO UPDATE SET
    price_cents = EXCLUDED.price_cents,
    stream_hours_month = EXCLUDED.stream_hours_month,
    max_concurrent_streams = EXCLUDED.max_concurrent_streams;

-- Retired tiers (unreferenced per the guard above).
DELETE FROM plans WHERE id IN ('pro-5x', 'pro-20x');

-- Daily fold retired (ADR 0002 rule 5 / ADR 0004): no second authority.
DROP TABLE IF EXISTS usage_ledger;

-- Trial retired (ADR 0005): no trial eligibility or trial window remains.
ALTER TABLE billing_accounts DROP COLUMN IF EXISTS trial_started_at;
ALTER TABLE subscriptions DROP COLUMN IF EXISTS trial_end;

-- Live-subscription slot without 'trialing' (ADR 0002 rule 4 / ADR 0005):
-- the five nonterminal known statuses reserve it; unknown statuses are
-- stored verbatim outside the predicate and fail closed at read time.
DROP INDEX IF EXISTS uq_subscriptions_live_slot;
CREATE UNIQUE INDEX IF NOT EXISTS uq_subscriptions_live_slot
    ON subscriptions (user_id)
    WHERE status IN ('incomplete', 'active', 'past_due', 'unpaid', 'paused');
