WITH candidates AS (
    SELECT id
    FROM billing_operations
    WHERE state IN ('pending', 'submitted', 'needs_reconcile')
    ORDER BY reconcile_attempted_at ASC NULLS FIRST, id
    LIMIT $1
    FOR UPDATE SKIP LOCKED
)
UPDATE billing_operations AS operation
SET reconcile_attempted_at = clock_timestamp()
FROM candidates
WHERE operation.id = candidates.id
RETURNING operation.id::text, operation.user_id, operation.kind, operation.state,
          operation.stripe_subscription_id, operation.request_payload::text AS request_payload
