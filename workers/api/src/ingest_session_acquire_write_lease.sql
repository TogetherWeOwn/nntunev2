-- $1..$8: authorized environment, library, principal, session, fingerprint,
-- generation, expected checkpoint revision and offset. $9: fresh server-only
-- lease ID. Use READ COMMITTED; a grant is usable only after confirmed commit.
-- No replacement or renewal: lost or uncertain ownership requires reconciliation,
-- not reissuing the lease for the same checkpoint. This grants no provider access.
WITH locked AS MATERIALIZED (
    SELECT session.id, session.idle_deadline, session.absolute_deadline
    FROM public.ingest_session_reservations AS session
    JOIN public.storage_library_namespaces AS library ON library.id = session.library_id
    WHERE library.environment_id = $1::text::uuid AND session.library_id = $2::text::uuid
      AND session.principal_id = $3 AND session.id = $4::text::uuid AND session.fingerprint = $5
      AND session.terminal_reason IS NULL AND session.write_lease_id IS NULL
      AND $9::text::uuid IS NOT NULL AND $6::text::uuid IS NOT NULL
      AND (session.checkpoint_generation IS NULL OR session.checkpoint_generation = $6::text::uuid)
      AND session.checkpoint_revision = $7 AND session.checkpoint_offset = $8
    FOR UPDATE OF session
), observed AS MATERIALIZED (
    SELECT locked.*, clock_timestamp() AS ts FROM locked
)
UPDATE public.ingest_session_reservations AS session
SET write_lease_id = $9::text::uuid, write_lease_generation = $6::text::uuid
FROM observed
WHERE session.id = observed.id
  AND observed.ts < LEAST(observed.idle_deadline, observed.absolute_deadline)
RETURNING session.write_lease_id;
