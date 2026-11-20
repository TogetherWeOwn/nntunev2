-- Exercise the schema independently of the API in the disposable migration job.
BEGIN;
INSERT INTO public.users (id, email, created_at) VALUES
    ('listen-a', 'listen-a@example.test', 1000),
    ('listen-b', 'listen-b@example.test', 1000);

INSERT INTO public.listen_events (user_id, playback_id, track_id, event, phase, recorded_at) VALUES
    ('listen-a', '40c325c0-2cee-489d-a8b6-fbe937155671', 't', 'start', 0, 1000),
    ('listen-a', '40c325c0-2cee-489d-a8b6-fbe937155671', 't', 'complete', 1, 2000),
    ('listen-b', '40c325c0-2cee-489d-a8b6-fbe937155671', 'other', 'start', 0, 1000);

-- One-byte track IDs are exercised above. In UTF-8 this track ID is 128 bytes.
INSERT INTO public.listen_events (user_id, playback_id, track_id, event, phase, recorded_at)
VALUES ('listen-a', '40c325c0-2cee-489d-a8b6-fbe937155673', repeat(U&'\00E9', 64), 'start', 0, 0);

DO $$ BEGIN
    BEGIN
        INSERT INTO public.listen_events (user_id, playback_id, track_id, event, phase, recorded_at)
        VALUES ('listen-a', '40c325c0-2cee-489d-a8b6-fbe937155671', 't', 'skip', 1, 3000);
        RAISE EXCEPTION 'schema allowed a second terminal event';
    EXCEPTION WHEN unique_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO public.listen_events (user_id, playback_id, track_id, event, phase, recorded_at)
        VALUES ('listen-b', '40c325c0-2cee-489d-a8b6-fbe937155672', 't', 'complete', 0, 3000);
        RAISE EXCEPTION 'schema allowed a terminal event in the start phase';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO public.listen_events (user_id, playback_id, track_id, event, phase, recorded_at)
        VALUES ('listen-a', 'not-a-uuid', 't', 'start', 0, 1000);
        RAISE EXCEPTION 'schema allowed a malformed playback ID';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO public.listen_events (user_id, playback_id, track_id, event, phase, recorded_at)
        VALUES ('listen-a', '40c325c0-2cee-489d-a8b6-fbe937155674', '', 'start', 0, 1000);
        RAISE EXCEPTION 'schema allowed an empty track ID';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO public.listen_events (user_id, playback_id, track_id, event, phase, recorded_at)
        VALUES ('listen-a', '40c325c0-2cee-489d-a8b6-fbe937155675', repeat(U&'\00E9', 64) || 't', 'start', 0, 1000);
        RAISE EXCEPTION 'schema allowed a 129-byte track ID';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO public.listen_events (user_id, playback_id, track_id, event, phase, recorded_at)
        VALUES ('listen-a', '40c325c0-2cee-489d-a8b6-fbe937155676', 't', 'start', 0, -1);
        RAISE EXCEPTION 'schema allowed a negative recorded time';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
END $$;

DELETE FROM public.users WHERE id = 'listen-a';
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM public.listen_events WHERE user_id = 'listen-a')
       OR (SELECT count(*) FROM public.listen_events WHERE user_id = 'listen-b') <> 1 THEN
        RAISE EXCEPTION 'account erasure left history or crossed owners';
    END IF;
END $$;
ROLLBACK;
