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
    terminal_before jsonb;
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

    BEGIN
        IF (SELECT pg_temp.terminate_session(f.environment_id, f.library_id, owner, sid, 'expire'))
            IS DISTINCT FROM 'expired' THEN
            RAISE EXCEPTION 'fixture failed to record expiry';
        END IF;
        SELECT to_jsonb(session) INTO STRICT terminal_before
        FROM public.ingest_session_reservations AS session WHERE id=sid;
        SELECT * INTO STRICT observed FROM pg_temp.session_status(f.environment_id, f.library_id, owner, sid);
        IF observed.state IS DISTINCT FROM 'expired' OR observed.expires_at IS DISTINCT FROM past OR
           (SELECT to_jsonb(session) FROM public.ingest_session_reservations AS session WHERE id=sid)
           IS DISTINCT FROM terminal_before THEN
            RAISE EXCEPTION 'status returned the wrong recorded expiry or changed the reservation';
        END IF;
        RAISE SQLSTATE 'Z0001';
    EXCEPTION WHEN SQLSTATE 'Z0001' THEN NULL;
    END;

    UPDATE public.ingest_session_reservations SET idle_deadline=idle_at WHERE id=sid;
    IF (SELECT pg_temp.terminate_session(f.environment_id, f.library_id, owner, sid, 'abort'))
        IS DISTINCT FROM 'aborted' THEN
        RAISE EXCEPTION 'fixture failed to abort an active session';
    END IF;
    UPDATE public.ingest_session_reservations SET idle_deadline=past WHERE id=sid;
    SELECT to_jsonb(session) INTO STRICT terminal_before
    FROM public.ingest_session_reservations AS session WHERE id=sid;
    SELECT * INTO STRICT observed FROM pg_temp.session_status(f.environment_id, f.library_id, owner, sid);
    IF observed.state IS DISTINCT FROM 'aborted' OR observed.expires_at IS DISTINCT FROM past OR
       (SELECT to_jsonb(session) FROM public.ingest_session_reservations AS session WHERE id=sid)
       IS DISTINCT FROM terminal_before THEN
        RAISE EXCEPTION 'status returned the wrong recorded abort or changed the reservation';
    END IF;
END $$;

DO $$
DECLARE
    f digest_lookup_fixture;
    owner CONSTANT text := 'session-owner';
    past CONSTANT timestamptz := statement_timestamp() - interval '1 hour';
    future CONSTANT timestamptz := statement_timestamp() + interval '1 hour';
    aborted_at CONSTANT timestamptz := past - interval '1 minute';
    test_case record;
    sid uuid;
    observed record;
    reservation_before public.ingest_session_reservations;
    cleanup_before jsonb;
BEGIN
    SELECT * INTO STRICT f FROM digest_lookup_fixture WHERE n=1;
    -- Checkpoint renewal caps idle_deadline at absolute_deadline. Match the
    -- two elapsed cases on both deadlines so only the recorded abort wins.
    FOR test_case IN SELECT * FROM (VALUES
        ('active', future, NULL::text, NULL::timestamptz),
        ('expired', past, NULL, NULL),
        ('aborted', past, 'aborted', aborted_at)
    ) AS cases (state, deadline, terminal_reason, terminal_at)
    LOOP
        sid := gen_random_uuid();
        INSERT INTO public.ingest_session_reservations
            (id, library_id, principal_id, idempotency_key, fingerprint,
             idle_deadline, absolute_deadline, terminal_reason, terminal_at)
        VALUES (sid, f.library_id, owner, 'status-capped-' || test_case.state,
                decode(repeat('ef', 32), 'hex'), test_case.deadline, test_case.deadline,
                test_case.terminal_reason, test_case.terminal_at)
        RETURNING * INTO STRICT reservation_before;
        INSERT INTO public.ingest_session_cleanup_intents (session_id, kind, due_at)
        VALUES (sid, 'absolute_expiry', test_case.deadline);
        IF test_case.terminal_reason IS NOT NULL THEN
            INSERT INTO public.ingest_session_cleanup_intents (session_id, kind, due_at)
            VALUES (sid, 'terminal_reconcile', test_case.terminal_at);
        END IF;
        SELECT jsonb_agg(intent ORDER BY intent.kind) INTO cleanup_before
        FROM public.ingest_session_cleanup_intents AS intent WHERE session_id=sid;

        SELECT * INTO STRICT observed FROM pg_temp.session_status(f.environment_id, f.library_id, owner, sid);
        IF observed.state IS DISTINCT FROM test_case.state OR observed.expires_at IS DISTINCT FROM test_case.deadline THEN
            RAISE EXCEPTION 'wrong capped-deadline status for expected state %', test_case.state;
        END IF;
        IF (SELECT session FROM public.ingest_session_reservations AS session WHERE id=sid)
           IS DISTINCT FROM reservation_before OR
           (SELECT jsonb_agg(intent ORDER BY intent.kind)
            FROM public.ingest_session_cleanup_intents AS intent WHERE session_id=sid) IS DISTINCT FROM cleanup_before THEN
            RAISE EXCEPTION 'capped-deadline status altered stored state for expected state %', test_case.state;
        END IF;
    END LOOP;
END $$;
\echo ingest session status fixture passed
