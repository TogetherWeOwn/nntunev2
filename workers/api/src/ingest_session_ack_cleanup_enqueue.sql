-- $1..$4: same trusted scope as claim; $5..$6: kind and delivery claim ID.
-- Run only after queue send succeeds. No row means this attempt did not record
-- the handoff. The enqueue acknowledgement says nothing about deletion.
WITH locked AS MATERIALIZED (
    SELECT intent.session_id, intent.kind, intent.claimed_until
    FROM public.ingest_session_reservations AS session
    JOIN public.storage_library_namespaces AS library ON library.id = session.library_id
    JOIN public.ingest_session_cleanup_intents AS intent ON intent.session_id = session.id
    WHERE library.environment_id = $1 AND session.library_id = $2
      AND session.principal_id = $3 AND session.id = $4 AND intent.kind = $5
      AND intent.delivery_claim_id = $6 AND intent.enqueued_at IS NULL
    FOR UPDATE OF intent
), observed AS MATERIALIZED (
    SELECT locked.*, clock_timestamp() AS ts FROM locked
)
UPDATE public.ingest_session_cleanup_intents AS intent
SET enqueued_at = observed.ts, delivery_claim_id = NULL, claimed_until = NULL
FROM observed
WHERE intent.session_id = observed.session_id AND intent.kind = observed.kind
  AND observed.ts < observed.claimed_until
RETURNING intent.enqueued_at;
