WITH candidates AS (
    SELECT id
    FROM subscriptions
    WHERE status IN ('incomplete', 'active', 'past_due', 'unpaid', 'paused')
    ORDER BY reconcile_attempted_at ASC NULLS FIRST, id
    LIMIT $1
    FOR UPDATE SKIP LOCKED
)
UPDATE subscriptions AS subscription
SET reconcile_attempted_at = clock_timestamp()
FROM candidates
WHERE subscription.id = candidates.id
RETURNING subscription.user_id, subscription.stripe_subscription_id
