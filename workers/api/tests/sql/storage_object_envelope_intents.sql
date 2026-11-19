-- Synthetic context bytes and an unauthenticated placeholder suffix.
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
     'e0000000-0000-4000-8000-000000000001', 65537, 65617, decode(repeat('ab', 48), 'hex')),
    ('c0000000-0000-4000-8000-000000000002', 'd0000000-0000-4000-8000-000000000002',
     'a0000000-0000-4000-8000-000000000001', 'b0000000-0000-4000-8000-000000000001',
     'e0000000-0000-4000-8000-000000000001', 65536, 65600, decode(repeat('ab', 48), 'hex'));

DO $$
DECLARE
    record_object_id CONSTANT UUID := 'c0000000-0000-4000-8000-000000000001';
    root_id CONSTANT UUID := 'f0000000-0000-4000-8000-000000000001';
    invalid_root_id CONSTANT UUID := 'f0000000-0000-5000-8000-000000000001';
    successor CONSTANT UUID := 'f0000000-0000-4000-8000-000000000002';
    record CONSTANT BYTEA := decode(
        '4e4e544d454430310002' || 'f0000000000040008000000000000001'
        || 'a0000000000040008000000000000001' || 'b0000000000040008000000000000001'
        || 'c0000000000040008000000000000001' || 'd0000000000040008000000000000001'
        || 'e0000000000040008000000000000001' || '0000000000010001' || '0000000000010051'
        || repeat('ab', 48) || repeat('00', 12) || repeat('cd', 48), 'hex');
    boundary_object_id CONSTANT UUID := 'c0000000-0000-4000-8000-000000000002';
    boundary_geometry CONSTANT BYTEA := decode('00000000000100000000000000010040', 'hex');
    boundary_context CONSTANT BYTEA := decode('c0000000000040008000000000000002d0000000000040008000000000000002', 'hex');
    boundary_record CONSTANT BYTEA := overlay(
        overlay(record PLACING boundary_context FROM 59) PLACING boundary_geometry FROM 107);
    insert_sql CONSTANT TEXT := 'INSERT INTO public.storage_object_envelope_intents VALUES (%L, %L, %L)';
    byte_offset INTEGER;
BEGIN
    IF EXISTS (SELECT FROM public.storage_object_envelope_intents) THEN
        RAISE EXCEPTION 'migration must not seed envelope records';
    END IF;
    -- Mutate magic, scheme, each UUID, P, E, H and nonce independently.
    FOREACH byte_offset IN ARRAY ARRAY[0, 9, 10, 26, 42, 58, 74, 90, 113, 121, 122, 181] LOOP
        PERFORM pg_temp.expect_error(format(insert_sql, record_object_id, root_id,
            set_byte(record, byte_offset, get_byte(record, byte_offset) # 1)), '23514');
    END LOOP;
    -- Match the column and embedded UUID so the context check does not mask a non-v4 root.
    PERFORM pg_temp.expect_error(format(insert_sql, record_object_id, invalid_root_id,
        overlay(record PLACING uuid_send(invalid_root_id) FROM 11)), '23514');
    PERFORM pg_temp.expect_error(format(insert_sql, record_object_id, root_id, substring(record FROM 1 FOR 229)), '23514');
    PERFORM pg_temp.expect_error(format(insert_sql, record_object_id, root_id, record || decode('00', 'hex')), '23514');
    PERFORM pg_temp.expect_error(format(insert_sql, successor, root_id, record), '23503');
    -- Both geometries are valid, but only for their own immutable intents.
    PERFORM pg_temp.expect_error(format(insert_sql, record_object_id, root_id,
        overlay(record PLACING boundary_geometry FROM 107)), '23514');
    PERFORM pg_temp.expect_error(format(insert_sql, boundary_object_id, root_id,
        overlay(record PLACING boundary_context FROM 59)), '23514');
    IF EXISTS (SELECT FROM public.storage_object_envelope_intents) THEN
        RAISE EXCEPTION 'refused records must not persist';
    END IF;

    EXECUTE format(insert_sql, record_object_id, root_id, record);
    EXECUTE format(insert_sql, boundary_object_id, root_id, boundary_record);
    PERFORM pg_temp.expect_error(format(insert_sql, record_object_id, root_id, set_byte(record, 229, 0)), '23505');
    -- A distinct version can retain a separate inactive historical record.
    EXECUTE format(insert_sql, record_object_id, successor, set_byte(record, 25, 2));
    IF (SELECT count(*) FROM public.storage_object_envelope_intents) <> 3 OR NOT EXISTS (
        SELECT FROM public.storage_object_envelope_intents AS e
        WHERE e.object_id = record_object_id AND e.root_version_id = root_id AND e.wrapped_record = record
    ) THEN
        RAISE EXCEPTION 'historical envelope record was not retained unchanged';
    END IF;
END $$;

SELECT pg_temp.expect_error(s, '23514') FROM (VALUES
    ('UPDATE public.storage_object_envelope_intents SET wrapped_record = wrapped_record'),
    ('DELETE FROM public.storage_object_envelope_intents'),
    ('TRUNCATE public.storage_object_envelope_intents')
) AS cases(s);
ROLLBACK;
