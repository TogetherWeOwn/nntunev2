CREATE FUNCTION pg_temp.session_status(UUID, UUID, TEXT, UUID)
RETURNS TABLE (state TEXT, expires_at TIMESTAMPTZ) LANGUAGE sql VOLATILE AS :'ingest_session_status_sql';

DO $$
DECLARE
    f digest_lookup_fixture;
    owner CONSTANT text := 'session-owner';
    sid CONSTANT uuid := gen_random_uuid();
    past CONSTANT timestamptz := clock_timestamp() - interval '1 minute';
    idle_at timestamptz;
    absolute_at timestamptz;
    observed record;
BEGIN
    SELECT * INTO STRICT f FROM digest_lookup_fixture WHERE n=1;
    PERFORM pg_temp.reserve_session(f.environment_id, f.library_id, owner, 'status',
        decode(repeat('ef', 32), 'hex'), sid);
    SELECT idle_deadline, absolute_deadline INTO STRICT idle_at, absolute_at
    FROM public.ingest_session_reservations WHERE id=sid;

    SELECT * INTO STRICT observed FROM pg_temp.session_status(f.environment_id, f.library_id, owner, sid);
    IF observed.state IS DISTINCT FROM 'active' OR observed.expires_at IS DISTINCT FROM idle_at OR
       EXISTS (SELECT FROM pg_temp.session_status(f.environment_id, f.library_id, 'session-other', sid)) OR
       EXISTS (SELECT FROM pg_temp.session_status(gen_random_uuid(), f.library_id, owner, sid)) OR
       EXISTS (SELECT FROM pg_temp.session_status(f.environment_id, gen_random_uuid(), owner, sid)) OR
       EXISTS (SELECT FROM pg_temp.session_status(f.environment_id, f.library_id, owner, gen_random_uuid())) THEN
        RAISE EXCEPTION 'wrong active status or an out-of-scope result';
    END IF;
    IF NOT EXISTS (SELECT FROM public.ingest_session_reservations WHERE id=sid AND idle_deadline=idle_at
                   AND absolute_deadline=absolute_at AND terminal_reason IS NULL) THEN
        RAISE EXCEPTION 'active status altered the reservation';
    END IF;

    UPDATE public.ingest_session_reservations SET idle_deadline=past WHERE id=sid;
    SELECT * INTO STRICT observed FROM pg_temp.session_status(f.environment_id, f.library_id, owner, sid);
    IF observed.state IS DISTINCT FROM 'expired' OR observed.expires_at IS DISTINCT FROM past OR
       NOT EXISTS (SELECT FROM public.ingest_session_reservations WHERE id=sid AND idle_deadline=past
                   AND absolute_deadline=absolute_at AND terminal_reason IS NULL) THEN
        RAISE EXCEPTION 'status renewed a deadline or required/wrote an expiry tombstone';
    END IF;

    UPDATE public.ingest_session_reservations SET idle_deadline=idle_at WHERE id=sid;
    IF (SELECT pg_temp.terminate_session(f.environment_id, f.library_id, owner, sid, 'abort'))
        IS DISTINCT FROM 'aborted' THEN
        RAISE EXCEPTION 'fixture failed to abort an active session';
    END IF;
    UPDATE public.ingest_session_reservations SET idle_deadline=past WHERE id=sid;
    IF (SELECT state FROM pg_temp.session_status(f.environment_id, f.library_id, owner, sid))
        IS DISTINCT FROM 'aborted' THEN
        RAISE EXCEPTION 'status replaced the earlier terminal verdict with expiry';
    END IF;
END $$;
\echo ingest session status fixture passed
