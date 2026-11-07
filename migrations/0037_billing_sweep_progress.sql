-- Rotate bounded reconciliation batches even when Stripe fails or returns
-- unchanged state. Attempt order is not entitlement or payment authority.
SET search_path = public, pg_catalog;

ALTER TABLE public.subscriptions
    ADD COLUMN IF NOT EXISTS reconcile_attempted_at TIMESTAMPTZ NULL;
ALTER TABLE public.billing_operations
    ADD COLUMN IF NOT EXISTS reconcile_attempted_at TIMESTAMPTZ NULL;

CREATE INDEX IF NOT EXISTS ix_subscriptions_reconcile_attempt
    ON public.subscriptions (reconcile_attempted_at ASC NULLS FIRST, id)
    WHERE status IN ('incomplete', 'active', 'past_due', 'unpaid', 'paused');
CREATE INDEX IF NOT EXISTS ix_billing_operations_reconcile_attempt
    ON public.billing_operations (reconcile_attempted_at ASC NULLS FIRST, id)
    WHERE state IN ('pending', 'submitted', 'needs_reconcile');
