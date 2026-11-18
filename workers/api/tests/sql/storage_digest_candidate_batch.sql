-- Synthetic candidates are not trusted enrollment or publication evidence.
BEGIN;
INSERT INTO public.storage_environments VALUES ('00000000-0000-4000-8000-000000000001');
INSERT INTO public.storage_library_namespaces VALUES
    ('10000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-000000000001', NULL);
INSERT INTO public.storage_object_intents VALUES
    ('20000000-0000-4000-8000-000000000001', '30000000-0000-4000-8000-000000000001',
     '00000000-0000-4000-8000-000000000001', '10000000-0000-4000-8000-000000000001',
     '40000000-0000-4000-8000-000000000001', 16777217, 16781377, decode(repeat('ab',48),'hex'));
INSERT INTO public.storage_plaintext_digest_claim_intents
SELECT object_id, environment_id, library_id, plaintext_bytes, decode(repeat('ab',32),'hex')
FROM public.storage_object_intents;
SELECT public.reserve_storage_digest('20000000-0000-4000-8000-000000000001', 3600);
DO $$
DECLARE
    env UUID := '00000000-0000-4000-8000-000000000001';
    lib UUID := '10000000-0000-4000-8000-000000000001';
    obj UUID := '20000000-0000-4000-8000-000000000001';
    map UUID := '50000000-0000-4000-8000-000000000001';
    h BYTEA := decode(repeat('ab',32),'hex');
    rejected BOOLEAN;
BEGIN
    FOR i IN 1..8 LOOP
        rejected := false;
        BEGIN
            PERFORM public.store_storage_digest_candidate(
                CASE WHEN i = 7 THEN gen_random_uuid() ELSE env END,
                CASE WHEN i = 1 THEN gen_random_uuid() ELSE lib END,
                CASE WHEN i = 8 THEN gen_random_uuid() ELSE obj END, map,
                CASE WHEN i = 2 THEN 2 ELSE 1 END,
                CASE i WHEN 3 THEN ARRAY[h] WHEN 4 THEN ARRAY[h, NULL::BYTEA]
                       WHEN 5 THEN ARRAY[h, decode('00','hex')]
                       WHEN 6 THEN array_fill(h, ARRAY[2], ARRAY[0]) ELSE ARRAY[h,h] END);
        EXCEPTION WHEN check_violation THEN rejected := true;
        END;
        IF NOT rejected THEN RAISE EXCEPTION 'invalid candidate % accepted', i; END IF;
    END LOOP;
    IF EXISTS (SELECT FROM public.storage_digest_map_intents) THEN
        RAISE EXCEPTION 'rejection left candidate rows';
    END IF;
    -- Expiry is fenced even before another reservation is acquired.
    UPDATE public.storage_digest_reservations SET expires_at = clock_timestamp() - interval '1 second'
    WHERE object_id = obj AND generation = 1;
    rejected := false;
    BEGIN
        PERFORM public.store_storage_digest_candidate(env, lib, obj, map, 1, ARRAY[h,h]);
    EXCEPTION WHEN check_violation THEN rejected := true;
    END;
    IF NOT rejected OR EXISTS (SELECT FROM public.storage_digest_map_intents)
       OR EXISTS (SELECT FROM public.storage_digest_map_page_intents) THEN
        RAISE EXCEPTION 'expired reservation persisted candidate rows';
    END IF;
    -- A reacquired lease fences the previous generation, even for the same object.
    PERFORM public.release_storage_digest(obj, 1);
    IF public.reserve_storage_digest(obj, 3600) IS DISTINCT FROM 2 THEN
        RAISE EXCEPTION 'reservation generation did not advance';
    END IF;
    IF public.release_storage_digest(obj, 1) THEN
        RAISE EXCEPTION 'stale release changed the current reservation';
    END IF;
    rejected := false;
    BEGIN
        PERFORM public.store_storage_digest_candidate(env, lib, obj, map, 1, ARRAY[h,h]);
    EXCEPTION WHEN check_violation THEN rejected := true;
    END;
    IF NOT rejected OR EXISTS (SELECT FROM public.storage_digest_map_intents)
       OR EXISTS (SELECT FROM public.storage_digest_map_page_intents) THEN
        RAISE EXCEPTION 'stale generation persisted candidate rows';
    END IF;
    PERFORM public.store_storage_digest_candidate(env, lib, obj, map, 2, ARRAY[h, decode(repeat('cd',32),'hex')]);
    IF (SELECT count(*) FROM public.storage_digest_map_page_intents WHERE map_id = map) <> 2
       OR NOT EXISTS (SELECT FROM public.storage_digest_map_page_intents WHERE map_id = map AND page_index = 1 AND entry_count = 1 AND expected_hash = decode(repeat('cd',32),'hex')) THEN
        RAISE EXCEPTION 'candidate batch is incomplete or reordered';
    END IF;
    PERFORM public.release_storage_digest(obj, 2);
    rejected := false;
    BEGIN
        PERFORM public.store_storage_digest_candidate(env, lib, obj, map, 2, ARRAY[h,h]);
    EXCEPTION WHEN check_violation THEN rejected := true;
    END;
    IF NOT rejected THEN RAISE EXCEPTION 'released reservation accepted'; END IF;
END $$;
ROLLBACK;
