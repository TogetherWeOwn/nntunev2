CREATE FUNCTION pg_temp.claim_cleanup(UUID, UUID, TEXT, UUID)
RETURNS TABLE (session_id UUID, kind TEXT, delivery_claim_id UUID, delay_seconds INTEGER)
LANGUAGE sql VOLATILE AS :'ingest_session_claim_cleanup_sql';
CREATE FUNCTION pg_temp.ack_cleanup_enqueue(UUID, UUID, TEXT, UUID, TEXT, UUID)
RETURNS SETOF TIMESTAMPTZ LANGUAGE sql VOLATILE AS :'ingest_session_ack_cleanup_enqueue_sql';

DO $$
DECLARE
    f digest_lookup_fixture;
    owner CONSTANT text := 'session-owner';
    sid CONSTANT uuid := gen_random_uuid();
    expiry_id uuid;
    terminal_id uuid;
    retry_id uuid;
BEGIN
    SELECT * INTO STRICT f FROM digest_lookup_fixture WHERE n=1;
    PERFORM pg_temp.reserve_session(f.environment_id, f.library_id, owner, 'delivery',
        decode(repeat('cd', 32), 'hex'), sid);
    PERFORM pg_temp.terminate_session(f.environment_id, f.library_id, owner, sid, 'abort');
    IF EXISTS (SELECT FROM pg_temp.claim_cleanup(gen_random_uuid(), f.library_id, owner, sid)) OR
       EXISTS (SELECT FROM pg_temp.claim_cleanup(f.environment_id, gen_random_uuid(), owner, sid)) OR
       EXISTS (SELECT FROM pg_temp.claim_cleanup(f.environment_id, f.library_id, 'session-other', sid)) THEN
        RAISE EXCEPTION 'out-of-scope delivery claim';
    END IF;
    BEGIN
        UPDATE public.ingest_session_cleanup_intents SET due_at=clock_timestamp() + interval '25 hours'
        WHERE session_id=sid AND kind='absolute_expiry';
        IF (SELECT array_agg(kind) FROM pg_temp.claim_cleanup(f.environment_id, f.library_id, owner, sid))
            IS DISTINCT FROM ARRAY['terminal_reconcile'] THEN
            RAISE EXCEPTION 'claim did not honor the 24-hour scheduling cutoff';
        END IF;
        RAISE SQLSTATE 'Z0001';
    EXCEPTION WHEN SQLSTATE 'Z0001' THEN NULL;
    END;
    BEGIN
        IF (SELECT count(*) FROM pg_temp.claim_cleanup(f.environment_id, f.library_id, owner, sid)) <> 2 THEN
            RAISE EXCEPTION 'expected both scoped intents';
        END IF;
        RAISE SQLSTATE 'Z0001';
    EXCEPTION WHEN SQLSTATE 'Z0001' THEN NULL;
    END;
    -- Transaction rollback made both intents claimable again.
    WITH claimed AS MATERIALIZED (SELECT * FROM pg_temp.claim_cleanup(f.environment_id, f.library_id, owner, sid))
    SELECT e.delivery_claim_id, t.delivery_claim_id INTO STRICT expiry_id, terminal_id
    FROM claimed AS e CROSS JOIN claimed AS t
    WHERE e.session_id=sid AND e.kind='absolute_expiry' AND e.delay_seconds BETWEEN 1 AND 86400
      AND t.session_id=sid AND t.kind='terminal_reconcile' AND t.delay_seconds=0;

    IF EXISTS (SELECT FROM pg_temp.claim_cleanup(f.environment_id, f.library_id, owner, sid)) OR
       EXISTS (SELECT FROM pg_temp.ack_cleanup_enqueue(gen_random_uuid(), f.library_id, owner, sid, 'absolute_expiry', expiry_id)) OR
       EXISTS (SELECT FROM pg_temp.ack_cleanup_enqueue(f.environment_id, gen_random_uuid(), owner, sid, 'absolute_expiry', expiry_id)) OR
       EXISTS (SELECT FROM pg_temp.ack_cleanup_enqueue(f.environment_id, f.library_id, 'session-other', sid, 'absolute_expiry', expiry_id)) OR
       EXISTS (SELECT FROM pg_temp.ack_cleanup_enqueue(f.environment_id, f.library_id, owner, sid, 'absolute_expiry', terminal_id)) THEN
        RAISE EXCEPTION 'live claim duplicated or acknowledgement accepted the wrong scope/intent';
    END IF;
    IF NOT EXISTS (SELECT FROM pg_temp.ack_cleanup_enqueue(f.environment_id, f.library_id, owner, sid, 'absolute_expiry', expiry_id)) THEN
        RAISE EXCEPTION 'queue acknowledgement failed';
    END IF;
    IF EXISTS (SELECT FROM pg_temp.ack_cleanup_enqueue(f.environment_id, f.library_id, owner, sid, 'absolute_expiry', expiry_id)) THEN
        RAISE EXCEPTION 'queue acknowledgement replayed';
    END IF;

    UPDATE public.ingest_session_cleanup_intents SET claimed_until=clock_timestamp()
    WHERE session_id=sid AND kind='terminal_reconcile';
    IF EXISTS (SELECT FROM pg_temp.ack_cleanup_enqueue(f.environment_id, f.library_id, owner, sid, 'terminal_reconcile', terminal_id)) THEN
        RAISE EXCEPTION 'expired claim acknowledged';
    END IF;
    SELECT delivery_claim_id INTO STRICT retry_id FROM pg_temp.claim_cleanup(f.environment_id, f.library_id, owner, sid)
    WHERE kind='terminal_reconcile' AND delay_seconds=0;
    IF retry_id IS NULL OR retry_id=terminal_id OR
       EXISTS (SELECT FROM pg_temp.ack_cleanup_enqueue(f.environment_id, f.library_id, owner, sid, 'terminal_reconcile', terminal_id)) THEN
        RAISE EXCEPTION 'stale claim acknowledged the successor';
    END IF;
    IF NOT EXISTS (SELECT FROM pg_temp.ack_cleanup_enqueue(f.environment_id, f.library_id, owner, sid, 'terminal_reconcile', retry_id)) THEN
        RAISE EXCEPTION 'successor could not acknowledge the queue handoff';
    END IF;
    IF EXISTS (SELECT FROM pg_temp.claim_cleanup(f.environment_id, f.library_id, owner, sid)) OR
       (SELECT count(*) FROM public.ingest_session_cleanup_intents AS i
        JOIN public.ingest_session_reservations AS s ON s.id=i.session_id
        WHERE i.session_id=sid AND i.enqueued_at IS NOT NULL AND i.delivery_claim_id IS NULL AND i.claimed_until IS NULL
          AND i.due_at=CASE i.kind WHEN 'absolute_expiry' THEN s.absolute_deadline ELSE s.terminal_at END) <> 2 THEN
        RAISE EXCEPTION 'acknowledgement lost an intent, changed its deadline, or left it claimable';
    END IF;
END $$;
\echo ingest session scoped queue handoff assertions passed
