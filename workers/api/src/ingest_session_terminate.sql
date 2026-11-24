-- $1..$4: trusted environment, library, principal and session ID. $5 is
-- the server-selected action ('abort' for an authorized owner, 'expire'
-- for existing cleanup). READ COMMITTED; acknowledge only after commit.
-- No row means no transition; use scoped status for an idempotent response.
-- The tombstone fences progress; it is not a cleanup or publication receipt.
WITH locked AS MATERIALIZED (
    SELECT session.id, session.idle_deadline, session.absolute_deadline
    FROM public.ingest_session_reservations AS session
    JOIN public.storage_library_namespaces AS library ON library.id = session.library_id
    WHERE library.environment_id = $1 AND session.library_id = $2
      AND session.principal_id = $3 AND session.id = $4
      AND session.terminal_reason IS NULL AND $5 IN ('abort', 'expire')
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
        terminal_at = observed.ts
    FROM observed
    WHERE session.id = observed.id AND session.terminal_reason IS NULL
      AND ($5 = 'abort' OR observed.ts >= LEAST(observed.idle_deadline, observed.absolute_deadline))
    RETURNING session.id, session.terminal_reason, session.terminal_at
), cleanup_intent AS (
    INSERT INTO public.ingest_session_cleanup_intents (session_id, kind, due_at)
    SELECT id, 'terminal_reconcile', terminal_at FROM terminated
)
SELECT terminal_reason FROM terminated;
