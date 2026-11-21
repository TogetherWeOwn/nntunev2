CREATE FUNCTION pg_temp.acquire_write_lease(UUID, UUID, TEXT, UUID, BYTEA, UUID, BIGINT, BIGINT, UUID)
RETURNS SETOF UUID LANGUAGE sql VOLATILE AS :'ingest_session_acquire_write_lease_sql';

CREATE FUNCTION pg_temp.advance_checkpoint(UUID, UUID, TEXT, UUID, BYTEA, UUID, BIGINT, BIGINT, BIGINT, UUID)
RETURNS SETOF BIGINT LANGUAGE sql VOLATILE AS :'ingest_session_advance_checkpoint_sql';

DO $$
DECLARE
    f digest_lookup_fixture;
    owner CONSTANT text := 'session-owner';
    sid CONSTANT uuid := gen_random_uuid();
    gen CONSTANT uuid := gen_random_uuid();
    fp CONSTANT bytea := decode(repeat('de', 32), 'hex');
    original_idle CONSTANT timestamptz := clock_timestamp() + interval '10 minutes';
    cap CONSTANT timestamptz := clock_timestamp() + interval '15 minutes';
    renewed_idle timestamptz;
    lease_id uuid := gen_random_uuid();
    next_lease CONSTANT uuid := gen_random_uuid();
BEGIN
    SELECT * INTO STRICT f FROM digest_lookup_fixture WHERE n=1;
    PERFORM pg_temp.reserve_session(f.environment_id, f.library_id, owner, 'checkpoint', fp, sid);
    UPDATE public.ingest_session_reservations SET idle_deadline=original_idle WHERE id=sid;

    IF EXISTS (SELECT FROM pg_temp.advance_checkpoint(f.environment_id, f.library_id, owner, sid, fp, gen, 0, 0, 100, lease_id)) OR
       EXISTS (SELECT FROM pg_temp.acquire_write_lease(f.environment_id, f.library_id, 'session-other', sid, fp, gen, 0, 0, lease_id)) THEN
        RAISE EXCEPTION 'unleased progress or foreign lease was accepted';
    END IF;
    IF (SELECT pg_temp.acquire_write_lease(f.environment_id, f.library_id, owner, sid, fp, gen, 0, 0, lease_id))
        IS DISTINCT FROM lease_id THEN
        RAISE EXCEPTION 'first lease missing';
    END IF;
    IF EXISTS (SELECT FROM pg_temp.acquire_write_lease(f.environment_id, f.library_id, owner, sid, fp, gen, 0, 0, next_lease)) THEN
        RAISE EXCEPTION 'outstanding lease was replaced';
    END IF;

    BEGIN
        IF (SELECT pg_temp.advance_checkpoint(f.environment_id, f.library_id, owner, sid, fp, gen, 0, 0, 100, lease_id))
            IS DISTINCT FROM 1::bigint THEN
            RAISE EXCEPTION 'initial checkpoint advance failed';
        END IF;
        RAISE SQLSTATE 'Z0001';
    EXCEPTION WHEN SQLSTATE 'Z0001' THEN NULL;
    END;
    IF NOT EXISTS (SELECT FROM public.ingest_session_reservations WHERE id=sid AND checkpoint_revision=0
                   AND checkpoint_generation IS NULL AND checkpoint_offset=0 AND idle_deadline=original_idle) THEN
        RAISE EXCEPTION 'rollback left checkpoint progress or renewal behind';
    END IF;
    IF (SELECT pg_temp.advance_checkpoint(f.environment_id, f.library_id, owner, sid, fp, gen, 0, 0, 100, lease_id))
            IS DISTINCT FROM 1::bigint THEN
        RAISE EXCEPTION 'initial advance could not be retried after rollback';
    END IF;
    SELECT idle_deadline INTO STRICT renewed_idle FROM public.ingest_session_reservations WHERE id=sid;
    IF renewed_idle <= original_idle OR EXISTS (
        SELECT FROM public.ingest_session_reservations WHERE id=sid AND write_lease_id IS NOT NULL
    ) THEN
        RAISE EXCEPTION 'accepted progress did not consume the lease and renew the idle deadline';
    END IF;

    IF (SELECT pg_temp.acquire_write_lease(f.environment_id, f.library_id, owner, sid, fp, gen, 1, 100, next_lease))
        IS DISTINCT FROM next_lease THEN
        RAISE EXCEPTION 'next lease missing';
    END IF;
    IF EXISTS (SELECT FROM pg_temp.advance_checkpoint(f.environment_id, f.library_id, owner, sid, fp, gen, 1, 100, 200, lease_id)) THEN
        RAISE EXCEPTION 'spent lease still usable';
    END IF;
    lease_id := next_lease;
    IF EXISTS (SELECT FROM pg_temp.advance_checkpoint(f.environment_id, f.library_id, owner, sid, fp, gen, 0, 0, 100, lease_id)) OR
       EXISTS (SELECT FROM pg_temp.advance_checkpoint(f.environment_id, f.library_id, owner, sid, fp, gen, 0, 100, 200, lease_id)) OR
       EXISTS (SELECT FROM pg_temp.advance_checkpoint(f.environment_id, f.library_id, owner, sid, fp, gen, 1, 0, 200, lease_id)) OR
       EXISTS (SELECT FROM pg_temp.advance_checkpoint(f.environment_id, f.library_id, owner, sid, fp, gen, 1, 100, 100, lease_id)) OR
       EXISTS (SELECT FROM pg_temp.advance_checkpoint(f.environment_id, f.library_id, owner, sid, fp, gen, 1, 100, 8589934593, lease_id)) OR
       EXISTS (SELECT FROM pg_temp.advance_checkpoint(f.environment_id, f.library_id, owner, sid, fp, gen_random_uuid(), 1, 100, 200, lease_id)) OR
       EXISTS (SELECT FROM pg_temp.advance_checkpoint(f.environment_id, f.library_id, owner, sid, ''::bytea, gen, 1, 100, 200, lease_id)) OR
       EXISTS (SELECT FROM pg_temp.advance_checkpoint(f.environment_id, f.library_id, 'session-other', sid, fp, gen, 1, 100, 200, lease_id)) OR
       EXISTS (SELECT FROM pg_temp.advance_checkpoint(gen_random_uuid(), f.library_id, owner, sid, fp, gen, 1, 100, 200, lease_id)) OR
       EXISTS (SELECT FROM pg_temp.advance_checkpoint(f.environment_id, gen_random_uuid(), owner, sid, fp, gen, 1, 100, 200, lease_id)) THEN
        RAISE EXCEPTION 'an invalid advance was accepted';
    END IF;
    IF NOT EXISTS (SELECT FROM public.ingest_session_reservations WHERE id=sid AND checkpoint_generation=gen
                   AND checkpoint_revision=1 AND checkpoint_offset=100 AND idle_deadline=renewed_idle) THEN
        RAISE EXCEPTION 'a rejected advance changed progress or the deadline';
    END IF;

    UPDATE public.ingest_session_reservations SET absolute_deadline=cap, idle_deadline=original_idle WHERE id=sid;
    IF (SELECT pg_temp.advance_checkpoint(f.environment_id, f.library_id, owner, sid, fp, gen, 1, 100, 200, lease_id))
            IS DISTINCT FROM 2::bigint THEN
        RAISE EXCEPTION 'second checkpoint advance failed';
    END IF;
    IF NOT EXISTS (SELECT FROM public.ingest_session_reservations WHERE id=sid AND checkpoint_generation=gen
                   AND checkpoint_revision=2 AND checkpoint_offset=200 AND idle_deadline=cap AND absolute_deadline=cap) THEN
        RAISE EXCEPTION 'checkpoint advance did not cap renewal at absolute expiry';
    END IF;

    lease_id := gen_random_uuid();
    IF (SELECT pg_temp.acquire_write_lease(f.environment_id, f.library_id, owner, sid, fp, gen, 2, 200, lease_id))
        IS DISTINCT FROM lease_id THEN
        RAISE EXCEPTION 'third lease missing';
    END IF;
    UPDATE public.ingest_session_reservations SET idle_deadline=clock_timestamp() WHERE id=sid;
    IF EXISTS (SELECT FROM pg_temp.advance_checkpoint(f.environment_id, f.library_id, owner, sid, fp, gen, 2, 200, 300, lease_id)) THEN
        RAISE EXCEPTION 'idle-expired session progressed before tombstoning';
    END IF;
    UPDATE public.ingest_session_reservations SET idle_deadline=original_idle WHERE id=sid;
    IF (SELECT pg_temp.terminate_session(f.environment_id, f.library_id, owner, sid, 'abort')) IS DISTINCT FROM 'aborted' THEN
        RAISE EXCEPTION 'fixture failed to abort an active session';
    END IF;
    IF EXISTS (SELECT FROM pg_temp.advance_checkpoint(f.environment_id, f.library_id, owner, sid, fp, gen, 2, 200, 300, lease_id)) OR
       EXISTS (SELECT FROM pg_temp.acquire_write_lease(f.environment_id, f.library_id, owner, sid, fp, gen, 2, 200, gen_random_uuid())) THEN
        RAISE EXCEPTION 'terminal session acquired a lease or advanced';
    END IF;
    IF NOT EXISTS (SELECT FROM public.ingest_session_reservations WHERE id=sid AND terminal_reason='aborted'
                   AND checkpoint_revision=2 AND checkpoint_offset=200 AND idle_deadline=original_idle
                   AND write_lease_id IS NULL) THEN
        RAISE EXCEPTION 'terminal session lost its prior checkpoint';
    END IF;
END $$;
\echo ingest session checkpoint advance fixture passed
