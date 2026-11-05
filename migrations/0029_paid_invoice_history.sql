-- NNTune v2 migration 0029: paid-invoice history (Stripe core slice).
--
-- One row per confirmed paid service invoice, written by reconciliation
-- (webhook and API/inline paths share the function) for every invoice
-- that anchors coverage: user, Stripe invoice id, subscription id,
-- resolving price plan, service period, amount/currency, and Stripe's
-- paid-at. The invoice id is the primary key, so retried webhooks,
-- duplicate events, out-of-order deliveries, and lost-response replays
-- converge on exactly one row per invoice instead of double-counting.
--
-- Replay safety: CREATE TABLE/INDEX IF NOT EXISTS (the migration gate
-- reapplies each file). New ids are the Stripe invoice ids (TEXT, the
-- provider's own key, not a generated UUID); new timestamps are
-- timestamptz; the user FK is TEXT matching users.id with ON DELETE
-- CASCADE (the migration 0020/0021 convention; account erasure still
-- deletes billing rows explicitly through the ADR 0002 rule-3 gate
-- before the user row goes).

CREATE TABLE IF NOT EXISTS paid_invoice_history (
    stripe_invoice_id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    stripe_subscription_id TEXT NOT NULL,
    plan_id TEXT NOT NULL REFERENCES plans (id),
    period_start TIMESTAMPTZ NOT NULL,
    period_end TIMESTAMPTZ NOT NULL,
    amount_cents BIGINT NOT NULL CHECK (amount_cents >= 0),
    currency TEXT NOT NULL,
    paid_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Per-user invoice history lookup (account history, renewal audits).
CREATE INDEX IF NOT EXISTS ix_paid_invoice_history_user
    ON paid_invoice_history (user_id);
