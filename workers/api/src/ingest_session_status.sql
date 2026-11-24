-- $1..$4: trusted environment, library, principal and session ID, after
-- current authorization/revocation checks. No row: absent or out of scope.
-- A read-only snapshot, not a cleanup receipt or permission to resume/write.
SELECT COALESCE(session.terminal_reason, CASE
           WHEN clock_timestamp() >= LEAST(session.idle_deadline, session.absolute_deadline) THEN 'expired'
           ELSE 'active'
       END) AS state,
       LEAST(session.idle_deadline, session.absolute_deadline) AS expires_at
FROM public.ingest_session_reservations AS session
JOIN public.storage_library_namespaces AS library ON library.id = session.library_id
WHERE library.environment_id = $1 AND session.library_id = $2
  AND session.principal_id = $3 AND session.id = $4;
