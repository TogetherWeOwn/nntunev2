-- $1..$6: authorized environment, library, principal; validated key and
-- request fingerprint; fresh server-generated session ID. Use READ COMMITTED
-- and return the identity only after confirmed commit. No row means refusal.
-- Replaying a reservation does not renew it or authorize any provider action.
WITH now AS MATERIALIZED (SELECT clock_timestamp() AS ts), reserved AS (
    INSERT INTO public.ingest_session_reservations AS session
        (id, library_id, principal_id, idempotency_key, fingerprint, absolute_deadline, idle_deadline)
    SELECT $6, library.id, $3, $4, $5, now.ts + interval '24 hours', now.ts + interval '60 minutes'
    FROM public.storage_library_namespaces AS library CROSS JOIN now
    WHERE library.environment_id = $1 AND library.id = $2
    ON CONFLICT (library_id, principal_id, idempotency_key) DO UPDATE
        SET idempotency_key = session.idempotency_key
        WHERE session.fingerprint = EXCLUDED.fingerprint
    RETURNING session.id, session.absolute_deadline
), expiry_intent AS (
    INSERT INTO public.ingest_session_cleanup_intents (session_id, kind, due_at)
    SELECT id, 'absolute_expiry', absolute_deadline FROM reserved
    ON CONFLICT (session_id, kind) DO NOTHING
)
SELECT id FROM reserved;
