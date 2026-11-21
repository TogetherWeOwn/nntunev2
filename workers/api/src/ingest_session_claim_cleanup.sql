-- Internal queue handoff. $1..$4: trusted environment, library, principal,
-- and session. At most two rows. Commit before sending to the ingest queue;
-- an ambiguous send can retry after the claim expires, so delivery is at least once.
WITH locked AS MATERIALIZED (
    SELECT intent.session_id, intent.kind, intent.due_at
    FROM public.ingest_session_reservations AS session
    JOIN public.storage_library_namespaces AS library ON library.id = session.library_id
    JOIN public.ingest_session_cleanup_intents AS intent ON intent.session_id = session.id
    WHERE library.environment_id = $1::text::uuid AND session.library_id = $2::text::uuid
      AND session.principal_id = $3 AND session.id = $4::text::uuid AND intent.enqueued_at IS NULL
      AND (intent.claimed_until IS NULL OR intent.claimed_until <= clock_timestamp())
    FOR UPDATE OF intent SKIP LOCKED
), observed AS MATERIALIZED (
    SELECT locked.*, clock_timestamp() AS ts FROM locked
)
UPDATE public.ingest_session_cleanup_intents AS intent
SET delivery_claim_id = gen_random_uuid(), claimed_until = observed.ts + interval '1 minute'
FROM observed
WHERE intent.session_id = observed.session_id AND intent.kind = observed.kind
  AND observed.due_at <= observed.ts + interval '24 hours'
RETURNING intent.session_id::text AS session_id, intent.kind,
    intent.delivery_claim_id::text AS delivery_claim_id,
    GREATEST(0, CEIL(EXTRACT(EPOCH FROM observed.due_at - observed.ts)))::int AS delay_seconds;
