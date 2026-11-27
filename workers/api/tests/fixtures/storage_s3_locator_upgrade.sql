-- Rehearse the unregistered locator DDL against disposable PostgreSQL only.
-- Seed before a populated upgrade or after a clean apply and replay.
BEGIN;
DO $$
BEGIN
    IF to_regclass('public.storage_s3_locator_intents') IS NOT NULL THEN
        RAISE EXCEPTION 'locator schema must be absent before the rehearsal';
    END IF;
END $$;

\if :replay
:locator_schema
:locator_schema
\endif

CREATE FUNCTION pg_temp.expect_locator_error(statement TEXT, expected TEXT) RETURNS VOID LANGUAGE plpgsql AS $$
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
INSERT INTO public.storage_object_intents
SELECT ('c0000000-0000-4000-8000-00000000000' || n)::UUID,
       ('d0000000-0000-4000-8000-00000000000' || n)::UUID,
       'a0000000-0000-4000-8000-000000000001', 'b0000000-0000-4000-8000-000000000001',
       'e0000000-0000-4000-8000-000000000001', 65537, 65617, decode(repeat('ab', 48), 'hex')
FROM generate_series(1, 3) AS s(n);
INSERT INTO public.storage_object_envelope_intents
SELECT object_id, root_id, decode('4e4e544d454430310002', 'hex') || uuid_send(root_id)
       || uuid_send(environment_id) || uuid_send(library_id) || uuid_send(object_id)
       || uuid_send(generation_id) || uuid_send(provider_configuration_version_id)
       || int8send(plaintext_bytes) || int8send(envelope_bytes) || header_tag
       || decode(repeat('00', 12) || repeat('cd', 48), 'hex')
FROM public.storage_object_intents
CROSS JOIN LATERAL (SELECT replace(object_id::text, 'c000', 'f000')::UUID AS root_id) r
WHERE object_id IN ('c0000000-0000-4000-8000-000000000001', 'c0000000-0000-4000-8000-000000000002');

INSERT INTO public.users (id, email, created_at)
VALUES ('user-locator-upgrade', 'locator-upgrade@example.com', 1700000000);
INSERT INTO public.ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
VALUES ('job_locator_upgrade', 'user-locator-upgrade',
        'ingest:user-locator-upgrade:' || repeat('a', 64) || ':upload', 1700000000, 'done');
INSERT INTO public.tracks (id, release_id, title, medium_no, track_no)
VALUES ('10000000-0000-4000-8000-000000000001', '20000000-0000-4000-8000-000000000001',
        'Legacy provenance', 1, 1);
INSERT INTO public.track_sources
    (track_id, source_job_id, nzb_ref_encrypted, nzb_ref_envelope_version, nzb_ref_kek_version,
     nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped, nzb_ref_payload_nonce, nzb_ref_payload_format)
VALUES ('10000000-0000-4000-8000-000000000001', 'job_locator_upgrade', decode('cafe', 'hex'), 2, 1,
        decode(repeat('11', 12), 'hex'), decode(repeat('22', 48), 'hex'), decode(repeat('33', 12), 'hex'), 1);
CREATE TEMP TABLE legacy_source_snapshot AS
SELECT to_jsonb(s) AS row_data FROM public.track_sources s WHERE source_job_id = 'job_locator_upgrade';

\if :replay
\else
:locator_schema
\endif

DO $$
DECLARE
    object_one CONSTANT UUID := 'c0000000-0000-4000-8000-000000000001';
    object_two CONSTANT UUID := 'c0000000-0000-4000-8000-000000000002';
    object_three CONSTANT UUID := 'c0000000-0000-4000-8000-000000000003';
    root_one CONSTANT UUID := 'f0000000-0000-4000-8000-000000000001';
    root_two CONSTANT UUID := 'f0000000-0000-4000-8000-000000000002';
    root_three CONSTANT UUID := 'f0000000-0000-4000-8000-000000000003';
    key_one CONSTANT TEXT := 'media/c0000000000040008000000000000001/d0000000000040008000000000000001';
    key_two CONSTANT TEXT := 'media/c0000000000040008000000000000002/d0000000000040008000000000000002';
    key_three CONSTANT TEXT := 'media/c0000000000040008000000000000003/d0000000000040008000000000000003';
    insert_sql CONSTANT TEXT := 'INSERT INTO public.storage_s3_locator_intents VALUES (%L,%L,%L,%L,%L,%L)';
    sample TEXT;
    locator_snapshot JSONB;
    intent_snapshot JSONB;
    envelope_snapshot JSONB;
BEGIN
    IF EXISTS (SELECT FROM public.storage_s3_locator_intents) THEN
        RAISE EXCEPTION 'migration must not invent locations';
    END IF;
    IF (SELECT to_jsonb(s) FROM public.track_sources s
        WHERE track_id = '10000000-0000-4000-8000-000000000001')
        IS DISTINCT FROM (SELECT row_data FROM legacy_source_snapshot) THEN
        RAISE EXCEPTION 'upgrade changed or removed legacy track provenance';
    END IF;
    IF EXISTS (SELECT FROM pg_class c CROSS JOIN LATERAL aclexplode(c.relacl) a
               WHERE c.oid = 'public.storage_s3_locator_intents'::regclass AND a.grantee = 0) THEN
        RAISE EXCEPTION 'locator metadata must not be granted to PUBLIC';
    END IF;

    PERFORM pg_temp.expect_locator_error(format(insert_sql, object_one, root_two, 'media-test', key_one, 'ver+1/=', NULL), '23503');
    PERFORM pg_temp.expect_locator_error(format(insert_sql, object_three, root_three, 'media-test', key_three, 'ver+3/=', NULL), '23503');
    FOREACH sample IN ARRAY ARRAY[
        key_two, upper(key_one), key_one || '/extra', replace(key_one, '/d', '/e'),
        '../' || key_one, '/' || key_one, key_one || '/..',
        'media/c0000000000040008000000000000001/../d0000000000040008000000000000001',
        'media/%2e%2e/d0000000000040008000000000000001'
    ] LOOP
        PERFORM pg_temp.expect_locator_error(format(insert_sql, object_one, root_one, 'media-test', sample, 'ver+1/=', NULL), '23514');
    END LOOP;
    FOREACH sample IN ARRAY ARRAY['a', repeat('a', 64), '01.2.3.4', 'bad..bucket', 'bad.-bucket', 'Bad-bucket', 'bad/bucket', '../bucket'] LOOP
        PERFORM pg_temp.expect_locator_error(format(insert_sql, object_one, root_one, sample, key_one, 'ver+1/=', NULL), '23514');
    END LOOP;
    FOREACH sample IN ARRAY ARRAY['', 'null', 'two words', E'pin\n', repeat('v', 1025)] LOOP
        PERFORM pg_temp.expect_locator_error(format(insert_sql, object_one, root_one, 'media-test', key_one, sample, NULL), '23514');
    END LOOP;
    FOREACH sample IN ARRAY ARRAY['opaque', '""', 'W/"weak"', '"a"b"', '"a b"', '"' || repeat('x', 1025) || '"'] LOOP
        PERFORM pg_temp.expect_locator_error(format(insert_sql, object_one, root_one, 'media-test', key_one, NULL, sample), '23514');
    END LOOP;
    PERFORM pg_temp.expect_locator_error(format(insert_sql, object_one, root_one, 'media-test', key_one, NULL, NULL), '23514');
    PERFORM pg_temp.expect_locator_error(format(insert_sql, object_one, root_one, 'media-test', key_one, 'ver+1/=', '"opaque"'), '23514');
    IF EXISTS (SELECT FROM public.storage_s3_locator_intents) THEN
        RAISE EXCEPTION 'rejected locator inserts must not persist rows';
    END IF;

    EXECUTE format(insert_sql, object_one, root_one, 'media-test.v1', key_one, 'ver+1/=', NULL);
    EXECUTE format(insert_sql, object_two, root_two, 'media-test.v1', key_two, NULL, '"opaque-9"');

    SELECT jsonb_agg(to_jsonb(l) ORDER BY l.object_id) INTO locator_snapshot FROM public.storage_s3_locator_intents l;
    SELECT jsonb_agg(to_jsonb(i) ORDER BY i.object_id) INTO intent_snapshot FROM public.storage_object_intents i;
    SELECT jsonb_agg(to_jsonb(e) ORDER BY e.object_id, e.root_version_id) INTO envelope_snapshot FROM public.storage_object_envelope_intents e;

    PERFORM pg_temp.expect_locator_error(format(insert_sql, object_one, root_one, 'other-bucket', key_one, 'different', NULL), '23505');
    EXECUTE format(insert_sql || ' ON CONFLICT (object_id) DO NOTHING',
                   object_one, root_one, 'other-bucket', key_one, 'different', NULL);
    PERFORM pg_temp.expect_locator_error(
        format(insert_sql || ' ON CONFLICT (object_id) DO UPDATE SET s3_bucket = EXCLUDED.s3_bucket',
               object_one, root_one, 'other-bucket', key_one, 'different', NULL),
        '23514'
    );
    PERFORM pg_temp.expect_locator_error(format(insert_sql, object_one, root_one, 'bad/bucket', key_one, 'different', NULL), '23514');
    PERFORM pg_temp.expect_locator_error(format(insert_sql, object_one, root_one, 'media-test.v1', key_one || '/..', 'different', NULL), '23514');
    PERFORM pg_temp.expect_locator_error('UPDATE public.storage_s3_locator_intents SET s3_bucket = ''other-bucket''', '23514');
    PERFORM pg_temp.expect_locator_error('UPDATE public.storage_s3_locator_intents SET s3_etag = s3_etag', '23514');
    PERFORM pg_temp.expect_locator_error('DELETE FROM public.storage_s3_locator_intents', '23514');
    PERFORM pg_temp.expect_locator_error('TRUNCATE public.storage_s3_locator_intents', '23514');

    IF (SELECT count(*) FROM public.storage_s3_locator_intents) <> 2 OR NOT EXISTS (
        SELECT FROM public.storage_s3_locator_intents l
        JOIN public.storage_object_envelope_intents e USING (object_id, root_version_id)
        JOIN public.storage_object_intents i USING (object_id)
        WHERE l.object_id = object_one AND l.s3_bucket = 'media-test.v1' AND l.s3_key = key_one
          AND l.s3_version_id = 'ver+1/=' AND l.s3_etag IS NULL
          AND i.plaintext_bytes = 65537 AND i.envelope_bytes = 65617
          AND substring(e.wrapped_record FROM 123 FOR 48) = i.header_tag
    ) OR NOT EXISTS (
        SELECT FROM public.storage_s3_locator_intents l
        JOIN public.storage_object_envelope_intents e USING (object_id, root_version_id)
        JOIN public.storage_object_intents i USING (object_id)
        WHERE l.object_id = object_two AND l.s3_bucket = 'media-test.v1' AND l.s3_key = key_two
          AND l.s3_version_id IS NULL AND l.s3_etag = '"opaque-9"'
          AND i.plaintext_bytes = 65537 AND i.envelope_bytes = 65617
          AND substring(e.wrapped_record FROM 123 FOR 48) = i.header_tag
    ) THEN
        RAISE EXCEPTION 'locator pins and bound envelope geometry did not round trip';
    END IF;
    IF (SELECT jsonb_agg(to_jsonb(l) ORDER BY l.object_id) FROM public.storage_s3_locator_intents l) IS DISTINCT FROM locator_snapshot
       OR (SELECT jsonb_agg(to_jsonb(i) ORDER BY i.object_id) FROM public.storage_object_intents i) IS DISTINCT FROM intent_snapshot
       OR (SELECT jsonb_agg(to_jsonb(e) ORDER BY e.object_id, e.root_version_id) FROM public.storage_object_envelope_intents e) IS DISTINCT FROM envelope_snapshot
       OR (SELECT to_jsonb(s) FROM public.track_sources s WHERE track_id = '10000000-0000-4000-8000-000000000001')
          IS DISTINCT FROM (SELECT row_data FROM legacy_source_snapshot) THEN
        RAISE EXCEPTION 'rejected locator mutations altered stored metadata';
    END IF;
END $$;
ROLLBACK;
