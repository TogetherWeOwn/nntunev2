-- These rows are inactive claims, not evidence of trusted enrollment.
BEGIN;
CREATE FUNCTION pg_temp.expect_error(statement TEXT, expected TEXT) RETURNS VOID LANGUAGE plpgsql AS $$
BEGIN
    BEGIN EXECUTE statement;
    EXCEPTION WHEN OTHERS THEN
        IF SQLSTATE = expected THEN RETURN; END IF;
        RAISE EXCEPTION 'expected %, got % for %', expected, SQLSTATE, statement;
    END;
    RAISE EXCEPTION 'expected % but succeeded: %', expected, statement;
END $$;
DO $$ BEGIN
    IF EXISTS (SELECT FROM public.storage_digest_map_intents) OR EXISTS (SELECT FROM public.storage_digest_map_page_intents) THEN
        RAISE EXCEPTION 'migration must not seed maps';
    END IF;
END $$;
INSERT INTO public.storage_environments VALUES ('00000000-0000-4000-8000-000000000001');
INSERT INTO public.storage_library_namespaces VALUES
    ('10000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-000000000001', NULL);
INSERT INTO public.storage_object_intents VALUES
    ('20000000-0000-4000-8000-000000000001', '30000000-0000-4000-8000-000000000001',
     '00000000-0000-4000-8000-000000000001', '10000000-0000-4000-8000-000000000001',
     '40000000-0000-4000-8000-000000000001', 16777217, 16781377, decode(repeat('ab',48),'hex'));
INSERT INTO public.storage_digest_map_intents (map_id, object_id, environment_id, library_id, plaintext_bytes) VALUES
    ('50000000-0000-4000-8000-000000000001', '20000000-0000-4000-8000-000000000001',
     '00000000-0000-4000-8000-000000000001', '10000000-0000-4000-8000-000000000001', 16777217);
INSERT INTO public.storage_digest_map_page_intents (map_id, chunk_count, page_index, expected_hash) VALUES
    ('50000000-0000-4000-8000-000000000001', 257, 0, decode(repeat('ab',32),'hex')),
    ('50000000-0000-4000-8000-000000000001', 257, 1, decode(repeat('cd',32),'hex'));
DO $$ BEGIN
    IF NOT EXISTS (SELECT FROM public.storage_digest_map_intents WHERE chunk_count = 257 AND page_count = 2)
       OR NOT EXISTS (SELECT FROM public.storage_digest_map_page_intents WHERE page_index = 0 AND first_chunk = 0 AND entry_count = 256)
       OR NOT EXISTS (SELECT FROM public.storage_digest_map_page_intents WHERE page_index = 1 AND first_chunk = 256 AND entry_count = 1) THEN
        RAISE EXCEPTION 'incorrect canonical geometry';
    END IF;
END $$;
SELECT pg_temp.expect_error(s, code) FROM (VALUES
    ($q$INSERT INTO public.storage_digest_map_page_intents (map_id,chunk_count,page_index,expected_hash) VALUES ('50000000-0000-4000-8000-000000000001',257,2,decode(repeat('ab',32),'hex'))$q$, '23514'),
    ($q$INSERT INTO public.storage_digest_map_page_intents (map_id,chunk_count,page_index,expected_hash) VALUES ('50000000-0000-4000-8000-000000000001',257,-1,decode(repeat('ab',32),'hex'))$q$, '23514'),
    ($q$INSERT INTO public.storage_digest_map_page_intents (map_id,chunk_count,page_index,expected_hash) VALUES ('50000000-0000-4000-8000-000000000001',257,0,decode(repeat('ab',32),'hex'))$q$, '23505'),
    ($q$INSERT INTO public.storage_digest_map_page_intents (map_id,chunk_count,page_index,expected_hash) VALUES ('50000000-0000-4000-8000-000000000001',256,0,decode(repeat('ab',32),'hex'))$q$, '23503'),
    ($q$INSERT INTO public.storage_digest_map_page_intents (map_id,chunk_count,page_index,expected_hash) VALUES ('50000000-0000-4000-8000-000000000002',257,0,decode(repeat('ab',31),'hex'))$q$, '23514'),
    ($q$INSERT INTO public.storage_digest_map_intents (map_id,object_id,environment_id,library_id,plaintext_bytes) VALUES ('50000000-0000-4000-8000-000000000002','20000000-0000-4000-8000-000000000001','00000000-0000-4000-8000-000000000001','10000000-0000-4000-8000-000000000001',1)$q$, '23505'),
    ($q$UPDATE public.storage_digest_map_intents SET plaintext_bytes = plaintext_bytes$q$, '23514'),
    ($q$DELETE FROM public.storage_digest_map_page_intents$q$, '23514'),
    ($q$TRUNCATE public.storage_digest_map_intents CASCADE$q$, '23514')
) AS cases(s, code);
ROLLBACK;
