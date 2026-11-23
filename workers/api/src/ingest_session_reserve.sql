-- $1..$6: authorized environment, library, principal; validated key and
-- request fingerprint; fresh server-generated session ID. Use READ COMMITTED
-- and return the identity only after confirmed commit. No row means refusal.
-- Replaying a reservation does not renew it or authorize any provider action.
WITH now AS MATERIALIZED (SELECT clock_timestamp() AS ts)
INSERT INTO public.ingest_session_reservations AS reserved
    (id, library_id, principal_id, idempotency_key, fingerprint, absolute_deadline, idle_deadline)
SELECT $6, library.id, $3, $4, $5, now.ts + interval '24 hours', now.ts + interval '60 minutes'
FROM public.storage_library_namespaces AS library CROSS JOIN now
WHERE library.environment_id = $1 AND library.id = $2
ON CONFLICT (library_id, principal_id, idempotency_key) DO UPDATE
    SET idempotency_key = reserved.idempotency_key
    WHERE reserved.fingerprint = EXCLUDED.fingerprint
RETURNING reserved.id;
