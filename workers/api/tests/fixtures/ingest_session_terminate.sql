-- Run the exact statement in the reservation fixture's rollback boundary.
CREATE FUNCTION pg_temp.terminate_session(UUID, UUID, TEXT, UUID, TEXT,
    BYTEA DEFAULT NULL, UUID DEFAULT NULL, BIGINT DEFAULT NULL, BIGINT DEFAULT NULL, UUID DEFAULT NULL)
RETURNS SETOF TEXT LANGUAGE sql VOLATILE AS :'ingest_session_terminate_sql';

DO $$
DECLARE
    f digest_lookup_fixture;
    owner CONSTANT text := 'session-owner';
    active_id CONSTANT uuid := gen_random_uuid();
    expired_id CONSTANT uuid := gen_random_uuid();
    request_fingerprint CONSTANT bytea := decode(repeat('cd', 32), 'hex');
    abort_time timestamptz;
BEGIN
    SELECT * INTO STRICT f FROM digest_lookup_fixture WHERE n=1;
    PERFORM pg_temp.reserve_session(f.environment_id, f.library_id, owner, 'active', request_fingerprint, active_id);
    PERFORM pg_temp.reserve_session(f.environment_id, f.library_id, owner, 'expired', request_fingerprint, expired_id);

    IF EXISTS (SELECT FROM pg_temp.terminate_session(f.environment_id, f.library_id, 'session-other', active_id, 'abort')) OR
       EXISTS (SELECT FROM pg_temp.terminate_session(gen_random_uuid(), f.library_id, owner, active_id, 'abort')) OR
       EXISTS (SELECT FROM pg_temp.terminate_session(f.environment_id, gen_random_uuid(), owner, active_id, 'abort')) OR
       EXISTS (SELECT FROM pg_temp.terminate_session(f.environment_id, f.library_id, owner, active_id, 'expire')) OR
       EXISTS (SELECT FROM pg_temp.terminate_session(f.environment_id, f.library_id, owner, active_id, 'complete')) THEN
        RAISE EXCEPTION 'out-of-scope, early expiry or unsupported action changed a session';
    END IF;

    IF EXISTS (SELECT FROM public.ingest_session_cleanup_intents
        WHERE session_id IN (active_id, expired_id) AND kind='terminal_reconcile') THEN
        RAISE EXCEPTION 'refused terminal action recorded cleanup work';
    END IF;

    BEGIN
        IF (SELECT pg_temp.terminate_session(f.environment_id, f.library_id, owner, active_id, 'abort'))
            IS DISTINCT FROM 'aborted' THEN
            RAISE EXCEPTION 'owned abort did not set the terminal fence';
        END IF;
        IF NOT EXISTS (SELECT FROM public.ingest_session_cleanup_intents
            WHERE session_id=active_id AND kind='terminal_reconcile') THEN
            RAISE EXCEPTION 'transaction did not see its own cleanup intent';
        END IF;
        RAISE SQLSTATE 'Z0001';
    EXCEPTION WHEN SQLSTATE 'Z0001' THEN
        NULL;
    END;
    IF EXISTS (SELECT FROM public.ingest_session_cleanup_intents
        WHERE session_id=active_id AND kind='terminal_reconcile') THEN
        RAISE EXCEPTION 'rolled-back abort left cleanup work';
    END IF;
    IF (SELECT pg_temp.terminate_session(f.environment_id, f.library_id, owner, active_id, 'abort'))
        IS DISTINCT FROM 'aborted' THEN
        RAISE EXCEPTION 'rolled-back abort left a terminal fence';
    END IF;
    SELECT terminal_at INTO STRICT abort_time FROM public.ingest_session_reservations WHERE id=active_id;
    IF abort_time IS NULL OR
       EXISTS (SELECT FROM pg_temp.terminate_session(f.environment_id, f.library_id, owner, active_id, 'abort')) OR
       (SELECT pg_temp.reserve_session(f.environment_id, f.library_id, owner, 'active', request_fingerprint,
            gen_random_uuid())) IS DISTINCT FROM active_id OR
       NOT EXISTS (SELECT FROM public.ingest_session_reservations WHERE id=active_id AND
            terminal_reason='aborted' AND terminal_at=abort_time) THEN
        RAISE EXCEPTION 'abort or create replay changed terminal state';
    END IF;

    UPDATE public.ingest_session_reservations SET idle_deadline=clock_timestamp() WHERE id=expired_id;
    IF (SELECT pg_temp.terminate_session(f.environment_id, f.library_id, owner, expired_id, 'expire'))
        IS DISTINCT FROM 'expired' OR
       EXISTS (SELECT FROM pg_temp.terminate_session(f.environment_id, f.library_id, owner, expired_id, 'abort')) THEN
        RAISE EXCEPTION 'idle expiry did not win or a late abort rewrote it';
    END IF;
    IF (SELECT count(*) FROM public.ingest_session_cleanup_intents AS intent
        JOIN public.ingest_session_reservations AS session ON session.id=intent.session_id
        WHERE session.id IN (active_id, expired_id) AND intent.kind='terminal_reconcile'
          AND intent.due_at=session.terminal_at) <> 2 THEN
        RAISE EXCEPTION 'abort and expiry did not each preserve one immediate reconciliation intent';
    END IF;
END $$;
\echo ingest session cleanup intent assertions passed
