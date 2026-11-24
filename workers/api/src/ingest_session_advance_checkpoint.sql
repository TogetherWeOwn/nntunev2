-- $1..$5: trusted environment, library, principal, session and request fingerprint.
-- $6..$9: trusted generation, expected checkpoint revision, expected and next plaintext offsets.
-- $10: server-only write lease ID, consumed by accepted progress.
-- A future caller must check authorization and declared input size, and commit
-- protected state and outbox changes in the same READ COMMITTED transaction.
-- No row means refusal.
-- The returned revision alone does not authorize resume or any provider action.
WITH locked AS MATERIALIZED (
    SELECT session.id, session.idle_deadline, session.absolute_deadline
    FROM public.ingest_session_reservations AS session
    JOIN public.storage_library_namespaces AS library ON library.id = session.library_id
    WHERE library.environment_id = $1 AND session.library_id = $2
      AND session.principal_id = $3 AND session.id = $4 AND session.fingerprint = $5
      AND session.terminal_reason IS NULL
      AND (session.multipart_object_id IS NULL OR session.multipart_upload_id IS NOT NULL)
      AND session.write_lease_id = $10 AND session.write_lease_generation = $6
      AND session.checkpoint_revision = $7 AND session.checkpoint_offset = $8
      AND $9 > $8 AND $9 <= 8589934592
    FOR UPDATE OF session
), observed AS MATERIALIZED (
    SELECT locked.*, clock_timestamp() AS ts FROM locked
)
UPDATE public.ingest_session_reservations AS session
SET checkpoint_generation = $6,
    checkpoint_revision = session.checkpoint_revision + 1,
    checkpoint_offset = $9,
    write_lease_id = NULL,
    write_lease_generation = NULL,
    idle_deadline = LEAST(observed.absolute_deadline, observed.ts + interval '60 minutes')
FROM observed
WHERE session.id = observed.id
  AND observed.ts < LEAST(observed.idle_deadline, observed.absolute_deadline)
RETURNING session.checkpoint_revision;
