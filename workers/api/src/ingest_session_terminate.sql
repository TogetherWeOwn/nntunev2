-- $1..$4: trusted environment, library, principal and session ID. $5 is
-- the server-selected action ('abort', 'expire', or internal 'checkpoint_lost').
-- For checkpoint loss, $6..$10 are the fingerprint, generation, revision,
-- offset and lease ID (nullable) of the snapshot that could not be restored.
-- Pass NULL for $6..$10 on owner abort or scheduled expiry. READ COMMITTED;
-- acknowledge only after commit. No row means no transition: re-read scoped
-- state, never turn a stale checkpoint-loss report into an unconditional abort.
-- The tombstone fences progress; it is not a cleanup or publication receipt.
WITH locked AS MATERIALIZED (
    SELECT session.id, session.idle_deadline, session.absolute_deadline
    FROM public.ingest_session_reservations AS session
    JOIN public.storage_library_namespaces AS library ON library.id = session.library_id
    WHERE library.environment_id = $1 AND session.library_id = $2
      AND session.principal_id = $3 AND session.id = $4
      AND session.terminal_reason IS NULL
      AND ($5 IN ('abort', 'expire') OR (
          $5 = 'checkpoint_lost' AND session.fingerprint = $6
          AND COALESCE(session.checkpoint_generation, session.write_lease_generation) = $7
          AND session.checkpoint_revision = $8 AND session.checkpoint_offset = $9
          AND session.write_lease_id IS NOT DISTINCT FROM $10
      ))
    FOR UPDATE OF session
), observed AS MATERIALIZED (
    -- Evaluate expiry after acquiring the row lock, not before a lock wait.
    SELECT locked.*, clock_timestamp() AS ts FROM locked
), terminated AS (
    UPDATE public.ingest_session_reservations AS session
    SET terminal_reason = CASE
            WHEN observed.ts >= LEAST(observed.idle_deadline, observed.absolute_deadline) THEN 'expired'
            ELSE 'aborted'
        END,
        terminal_at = observed.ts,
        write_lease_id = NULL,
        write_lease_generation = NULL
    FROM observed
    WHERE session.id = observed.id AND session.terminal_reason IS NULL
      AND ($5 <> 'expire' OR observed.ts >= LEAST(observed.idle_deadline, observed.absolute_deadline))
    RETURNING session.id, session.terminal_reason, session.terminal_at
), cleanup_intent AS (
    INSERT INTO public.ingest_session_cleanup_intents (session_id, kind, due_at)
    SELECT id, 'terminal_reconcile', terminal_at FROM terminated
)
SELECT terminal_reason FROM terminated;
