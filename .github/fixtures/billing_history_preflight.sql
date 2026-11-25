CREATE ROLE invoice_preflight_fixture LOGIN PASSWORD 'ci-throwaway';
CREATE SCHEMA billing_preflight;
CREATE TABLE billing_preflight.scope (
    stripe_invoice_id text PRIMARY KEY,
    user_id text NOT NULL,
    stripe_subscription_id text NOT NULL,
    retained boolean NOT NULL
);
CREATE TABLE billing_preflight.projection (
    stripe_invoice_id text PRIMARY KEY,
    user_id text NOT NULL,
    stripe_subscription_id text NOT NULL,
    evidence_complete boolean NOT NULL,
    plan_id text,
    start_s bigint,
    end_s bigint,
    amount_cents bigint,
    currency text,
    paid_s bigint,
    starts bigint[],
    ends bigint[]
);
GRANT USAGE ON SCHEMA billing_preflight TO invoice_preflight_fixture;
GRANT SELECT ON billing_preflight.scope, billing_preflight.projection TO invoice_preflight_fixture;
GRANT SELECT (user_id, stripe_subscription_id) ON public.subscriptions TO invoice_preflight_fixture;
GRANT SELECT (stripe_invoice_id, user_id, stripe_subscription_id, plan_id, period_start,
    period_end, amount_cents, currency, paid_at, covered_periods)
    ON public.paid_invoice_history TO invoice_preflight_fixture;

INSERT INTO public.users (id, email, created_at)
VALUES ('user_fixture', 'invoice-preflight@example.test', 1700000000),
       ('user_outside_trial', 'outside-trial@example.test', 1700000000);
INSERT INTO public.subscriptions (user_id, stripe_subscription_id, stripe_customer_id,
    stripe_subscription_item_id, stripe_price_id, plan_id, status)
VALUES ('user_fixture', 'sub_fixture', 'cus_fixture', 'item_fixture', 'price_fixture', 'plus', 'active'),
       ('user_outside_trial', 'sub_outside_trial', 'cus_outside_trial', 'item_outside_trial',
        'price_fixture', 'plus', 'active');

INSERT INTO billing_preflight.scope
SELECT 'in_fixture_' || n, 'user_fixture', 'sub_fixture', n <= 2
FROM generate_series(1, 3) AS n;
INSERT INTO billing_preflight.projection
SELECT stripe_invoice_id, user_id, stripe_subscription_id, true, 'plus',
       lo, hi, 1000, 'usd', lo, ARRAY[lo, lo + 1], ARRAY[lo + 2, hi]
FROM billing_preflight.scope
CROSS JOIN LATERAL (
    SELECT 1700000000::bigint + 2592000 * (right(stripe_invoice_id, 1)::int - 1) AS lo
) AS starts
CROSS JOIN LATERAL (SELECT lo + 2592000 AS hi) AS ends;
INSERT INTO public.paid_invoice_history (stripe_invoice_id, user_id, stripe_subscription_id,
    plan_id, period_start, period_end, amount_cents, currency, paid_at, covered_periods)
SELECT p.stripe_invoice_id, p.user_id, p.stripe_subscription_id, p.plan_id, to_timestamp(start_s),
       to_timestamp(end_s), amount_cents, currency, to_timestamp(paid_s),
       CASE WHEN p.stripe_invoice_id = 'in_fixture_1'
            THEN tstzmultirange(tstzrange(to_timestamp(start_s), to_timestamp(end_s), '[)'))
       END
FROM billing_preflight.projection AS p
JOIN billing_preflight.scope AS s USING (stripe_invoice_id)
WHERE s.retained;
