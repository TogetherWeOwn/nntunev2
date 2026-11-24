-- Included inside the digest lookup fixture's rolled-back transaction. Reuse
-- only its namespace identities; seeded map rows confer no ingest authority.
\ir ../../src/ingest_session_schema.sql
CREATE FUNCTION pg_temp.reserve_session(UUID, UUID, TEXT, TEXT, BYTEA, UUID)
RETURNS SETOF UUID LANGUAGE sql VOLATILE AS :'ingest_session_reserve_sql';

INSERT INTO public.users (id, email, created_at) VALUES
    ('session-owner', 'session-owner@example.invalid', 1),
    ('session-other', 'session-other@example.invalid', 1);

DO $$
DECLARE
    f digest_lookup_fixture;
    first_id CONSTANT uuid := gen_random_uuid();
    retry_id CONSTANT uuid := gen_random_uuid();
    private_library CONSTANT uuid := gen_random_uuid();
    private_id CONSTANT uuid := gen_random_uuid();
    key CONSTANT text := 'request-key';
    request_fingerprint CONSTANT bytea := decode(repeat('ab', 32), 'hex');
    expired CONSTANT timestamptz := clock_timestamp() - interval '1 hour';
    reserved public.ingest_session_reservations;
BEGIN
    SELECT * INTO STRICT f FROM digest_lookup_fixture WHERE n=1;
    IF EXISTS (SELECT FROM pg_temp.reserve_session(gen_random_uuid(), f.library_id,
        'session-owner', key, request_fingerprint, first_id)) THEN
        RAISE EXCEPTION 'environment mismatch reserved a session';
    END IF;
    BEGIN
        IF (SELECT pg_temp.reserve_session(f.environment_id, f.library_id,
            'session-owner', key, request_fingerprint, retry_id)) IS DISTINCT FROM retry_id THEN
            RAISE EXCEPTION 'transaction did not see its own reservation';
        END IF;
        RAISE SQLSTATE 'Z0001';
    EXCEPTION WHEN SQLSTATE 'Z0001' THEN
        NULL; -- Roll back the reservation before another transaction can observe it.
    END;
    IF EXISTS (SELECT FROM public.ingest_session_cleanup_intents) THEN
        RAISE EXCEPTION 'refused or rolled-back reservation left cleanup work';
    END IF;
    IF (SELECT pg_temp.reserve_session(f.environment_id, f.library_id,
        'session-owner', key, request_fingerprint, first_id)) IS DISTINCT FROM first_id THEN
        RAISE EXCEPTION 'create did not reserve its session identity';
    END IF;
    SELECT * INTO STRICT reserved FROM public.ingest_session_reservations WHERE id=first_id;
    IF reserved.absolute_deadline - reserved.idle_deadline <> interval '23 hours' OR
       reserved.idle_deadline <= clock_timestamp() THEN
        RAISE EXCEPTION 'wrong reservation deadline';
    END IF;
    IF NOT EXISTS (SELECT FROM public.ingest_session_cleanup_intents WHERE session_id=first_id
        AND kind='absolute_expiry' AND due_at=reserved.absolute_deadline) THEN
        RAISE EXCEPTION 'reservation did not record its original absolute expiry intent';
    END IF;
    -- Simulate an expired reservation. Replay is still the same identity, not renewal.
    UPDATE public.ingest_session_reservations SET idle_deadline=expired WHERE id=first_id;
    IF (SELECT pg_temp.reserve_session(f.environment_id, f.library_id,
        'session-owner', key, request_fingerprint, retry_id)) IS DISTINCT FROM first_id OR
       EXISTS (SELECT FROM pg_temp.reserve_session(f.environment_id, f.library_id,
        'session-owner', key, decode(repeat('00', 32), 'hex'), retry_id)) THEN
        RAISE EXCEPTION 'matching retry or changed fingerprint returned the wrong result';
    END IF;
    IF NOT EXISTS (SELECT FROM public.ingest_session_reservations WHERE id=first_id AND
        idle_deadline=expired AND absolute_deadline=reserved.absolute_deadline AND fingerprint=request_fingerprint) OR
       (SELECT count(*) FROM public.ingest_session_reservations) <> 1 OR
       (SELECT count(*) FROM public.ingest_session_cleanup_intents) <> 1 OR
       NOT EXISTS (SELECT FROM public.ingest_session_cleanup_intents WHERE session_id=first_id
           AND kind='absolute_expiry' AND due_at=reserved.absolute_deadline) THEN
        RAISE EXCEPTION 'replay or refusal changed the reservation or expiry intent';
    END IF;
    INSERT INTO public.storage_library_namespaces VALUES (private_library, f.environment_id, 'session-owner');
    IF (SELECT pg_temp.reserve_session(f.environment_id, f.library_id,
        'session-other', key, request_fingerprint, retry_id)) IS DISTINCT FROM retry_id OR
       (SELECT pg_temp.reserve_session(f.environment_id, private_library,
        'session-owner', key, request_fingerprint, private_id)) IS DISTINCT FROM private_id THEN
        RAISE EXCEPTION 'a different principal or library did not get an independent reservation';
    END IF;
END $$;
\echo 'test (hosted): ingest_session_reserve passed'
