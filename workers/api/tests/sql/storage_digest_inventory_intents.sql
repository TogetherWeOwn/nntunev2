-- Synthetic opaque versions; no provider or validation authority.
BEGIN;
CREATE FUNCTION pg_temp.expect_error(statement TEXT, expected TEXT) RETURNS VOID LANGUAGE plpgsql AS $$
BEGIN
    BEGIN EXECUTE statement;
    EXCEPTION WHEN OTHERS THEN
        IF SQLSTATE = expected THEN RETURN; END IF;
        RAISE EXCEPTION 'expected %, got %', expected, SQLSTATE;
    END;
    RAISE EXCEPTION 'expected % but succeeded', expected;
END $$;

INSERT INTO public.storage_environments VALUES ('a0000000-0000-4000-8000-000000000001');
INSERT INTO public.storage_library_namespaces VALUES
    ('b0000000-0000-4000-8000-000000000001', 'a0000000-0000-4000-8000-000000000001', NULL);
INSERT INTO public.storage_object_intents VALUES
    ('c0000000-0000-4000-8000-000000000001', 'd0000000-0000-4000-8000-000000000001',
     'a0000000-0000-4000-8000-000000000001', 'b0000000-0000-4000-8000-000000000001',
     'e0000000-0000-4000-8000-000000000001', 16777217, 16781377, decode(repeat('ab', 48), 'hex')),
    ('c0000000-0000-4000-8000-000000000002', 'd0000000-0000-4000-8000-000000000002',
     'a0000000-0000-4000-8000-000000000001', 'b0000000-0000-4000-8000-000000000001',
     'e0000000-0000-4000-8000-000000000001', 1, 65, decode(repeat('ab', 48), 'hex'));
INSERT INTO public.storage_digest_map_intents (map_id, object_id, environment_id, library_id, plaintext_bytes) VALUES
    ('f0000000-0000-4000-8000-000000000001', 'c0000000-0000-4000-8000-000000000001',
     'a0000000-0000-4000-8000-000000000001', 'b0000000-0000-4000-8000-000000000001', 16777217),
    ('f0000000-0000-4000-8000-000000000002', 'c0000000-0000-4000-8000-000000000002',
     'a0000000-0000-4000-8000-000000000001', 'b0000000-0000-4000-8000-000000000001', 1);

DO $$
DECLARE
    multi CONSTANT UUID := 'f0000000-0000-4000-8000-000000000001';
    single CONSTANT UUID := 'f0000000-0000-4000-8000-000000000002';
    unknown_map CONSTANT UUID := 'f0000000-0000-4000-8000-000000000003';
    insert_sql CONSTANT TEXT := 'INSERT INTO public.storage_digest_inventory_intents VALUES (%L, %L, %L)';
    versions CONSTANT TEXT[] := ARRAY['last-in-sort-order', 'first-in-sort-order'];
BEGIN
    PERFORM pg_temp.expect_error(format(insert_sql, unknown_map, 'media', versions), '23503');
    -- Having every version pin cannot substitute for the actual page intents.
    INSERT INTO public.storage_digest_map_page_intents (map_id, chunk_count, page_index, expected_hash)
    VALUES (multi, 257, 0, decode(repeat('ab', 32), 'hex')),
           (single, 1, 0, decode(repeat('cd', 32), 'hex'));
    PERFORM pg_temp.expect_error(format(insert_sql, multi, 'media', versions), '23514');
    INSERT INTO public.storage_digest_map_page_intents (map_id, chunk_count, page_index, expected_hash)
    VALUES (multi, 257, 1, decode(repeat('ef', 32), 'hex'));

    PERFORM pg_temp.expect_error(format(insert_sql, multi, 'media', pages), '23514') FROM (VALUES
        (NULL::TEXT), ('{}'), ('{first}'), ('{first,second,extra}'),
        ('[0:1]={first,second}'), ('{{first,second}}'), ('{first,NULL}'),
        ('{first,"null"}'), ('{first,""}'), (ARRAY['first', repeat('a', 1025)]::TEXT)
    ) AS cases(pages);
    PERFORM pg_temp.expect_error(format(insert_sql, multi, media, versions), '23514') FROM (VALUES
        (NULL::TEXT), (''), ('null'), (repeat('a', 1025))
    ) AS cases(media);
    IF EXISTS (SELECT FROM public.storage_digest_inventory_intents) THEN
        RAISE EXCEPTION 'refused inventory must not persist';
    END IF;

    EXECUTE format(insert_sql, multi, repeat('a', 1024), versions);
    EXECUTE format(insert_sql, single, NULL, ARRAY[NULL::TEXT]);
    PERFORM pg_temp.expect_error(format(insert_sql, multi, 'different', versions), '23505');
    IF NOT EXISTS (SELECT FROM public.storage_digest_inventory_intents
        WHERE map_id = multi AND media_version = repeat('a', 1024)
          AND page_versions[1] = versions[1] AND page_versions[2] = versions[2])
       OR NOT EXISTS (SELECT FROM public.storage_digest_inventory_intents
        WHERE map_id = single AND media_version IS NULL
          AND cardinality(page_versions) = 1 AND page_versions[1] IS NULL) THEN
        RAISE EXCEPTION 'inventory was not retained exactly';
    END IF;
END $$;

SELECT pg_temp.expect_error(s, '23514') FROM (VALUES
    ('UPDATE public.storage_digest_inventory_intents SET media_version = NULL'),
    ('DELETE FROM public.storage_digest_inventory_intents'),
    ('TRUNCATE public.storage_digest_inventory_intents')
) AS cases(s);
ROLLBACK;
