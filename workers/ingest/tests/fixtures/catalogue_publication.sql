-- Rehearsal schema for the service-owned one-source-per-occurrence slot.
-- The object keys immutable locator, winner, map and receipt records.
CREATE TABLE public.catalogue_s3_sources (
    track_id UUID PRIMARY KEY REFERENCES public.tracks (id),
    source_job_id TEXT REFERENCES public.ingest_jobs (id) ON DELETE SET NULL,
    accepted_seq BIGINT NOT NULL,
    object_id UUID NOT NULL REFERENCES public.storage_s3_locator_intents (object_id)
        REFERENCES public.storage_validated_digest_winners (object_id)
);

-- Disposable substitute for the session outcome writer. The read side resolves
-- the result through the real committed publisher rows.
CREATE TABLE public.test_committed_uploads (
    scope TEXT[] PRIMARY KEY,
    map_id UUID NOT NULL REFERENCES public.storage_digest_inventory_intents (map_id)
);

INSERT INTO public.users (id, email, created_at) VALUES
    ('catalogue-owner', 'catalogue@example.invalid', 1),
    ('catalogue-other', 'other@example.invalid', 1);
INSERT INTO public.ingest_jobs (id, user_id, idempotency_key, accepted_at, status) VALUES
    ('catalogue-old', 'catalogue-owner', 'first', 100, 'done'),
    ('catalogue-new', 'catalogue-owner', 'second', 10, 'done'),
    ('catalogue-queued', 'catalogue-owner', 'third', 101, 'queued');

-- The old object comes from the publisher regression. Inject an independent
-- synthetic winner in that same immutable scope to exercise pointer replacement.
CREATE FUNCTION pg_temp.catalogue_id(b TEXT) RETURNS UUID LANGUAGE sql AS $$
    SELECT (repeat(b, 6) || '40' || b || '80' || repeat(b, 7))::UUID
$$;
DO $$
DECLARE
    old_object CONSTANT UUID := pg_temp.catalogue_id('73');
    foreign_env CONSTANT UUID := pg_temp.catalogue_id('7f');
    foreign_library CONSTANT UUID := pg_temp.catalogue_id('80');
    private_library CONSTANT UUID := pg_temp.catalogue_id('81');
    candidate RECORD;
    digest CONSTANT BYTEA := decode(repeat('fd', 32), 'hex');
    prior public.storage_object_intents%ROWTYPE;
    envelope public.storage_object_envelope_intents%ROWTYPE;
BEGIN
    SELECT * INTO STRICT prior FROM public.storage_object_intents WHERE object_id = old_object;
    SELECT * INTO STRICT envelope FROM public.storage_object_envelope_intents WHERE object_id = old_object;
    INSERT INTO public.storage_environments VALUES (foreign_env);
    INSERT INTO public.storage_library_namespaces VALUES
        (foreign_library, foreign_env, NULL), (private_library, prior.environment_id, 'catalogue-other');
    FOR candidate IN
        SELECT environment_id, library_id, pg_temp.catalogue_id(object_byte) AS object_id,
               pg_temp.catalogue_id(generation_byte) AS generation_id, pg_temp.catalogue_id(map_byte) AS map_id
        FROM (VALUES (prior.environment_id, prior.library_id, '76', '77', '78'),
                     (foreign_env, foreign_library, '79', '7a', '7b'),
                     (prior.environment_id, private_library, '7c', '7d', '7e'))
        AS scope(environment_id, library_id, object_byte, generation_byte, map_byte)
    LOOP
        INSERT INTO public.storage_object_intents VALUES (
            candidate.object_id, candidate.generation_id, candidate.environment_id, candidate.library_id,
            prior.provider_configuration_version_id, prior.plaintext_bytes, prior.envelope_bytes, prior.header_tag);
        INSERT INTO public.storage_object_envelope_intents VALUES (
            candidate.object_id, envelope.root_version_id,
            substring(envelope.wrapped_record for 26)
            || uuid_send(candidate.environment_id) || uuid_send(candidate.library_id)
            || uuid_send(candidate.object_id) || uuid_send(candidate.generation_id)
            || substring(envelope.wrapped_record from 91));
        INSERT INTO public.storage_plaintext_digest_claim_intents VALUES (
            candidate.object_id, candidate.environment_id, candidate.library_id, prior.plaintext_bytes, digest);
        INSERT INTO public.storage_digest_map_intents (map_id, object_id, environment_id, library_id, plaintext_bytes)
            VALUES (candidate.map_id, candidate.object_id, candidate.environment_id, candidate.library_id, prior.plaintext_bytes);
        INSERT INTO public.storage_digest_map_page_intents (map_id, chunk_count, page_index, expected_hash)
            SELECT candidate.map_id, chunk_count, page_index, expected_hash FROM public.storage_digest_map_page_intents
            WHERE map_id = (SELECT map_id FROM public.storage_digest_map_intents WHERE object_id = old_object);
        INSERT INTO public.storage_digest_inventory_intents VALUES (candidate.map_id, 'new-media-pin', ARRAY['new-page-pin']);
        INSERT INTO public.storage_validated_digest_winners
            (object_id, environment_id, library_id, plaintext_bytes, plaintext_sha256, root_version_id, retain_until)
            SELECT candidate.object_id, candidate.environment_id, candidate.library_id,
                   plaintext_bytes, digest, root_version_id, retain_until
            FROM public.storage_validated_digest_winners WHERE object_id = old_object;
    END LOOP;
    INSERT INTO public.storage_s3_locator_intents
        SELECT w.object_id, w.root_version_id, 'test-bucket',
               'media/' || encode(uuid_send(i.object_id), 'hex') || '/' || encode(uuid_send(i.generation_id), 'hex'),
               p.media_version, NULL
        FROM public.storage_validated_digest_winners w
        JOIN public.storage_object_intents i USING (object_id)
        JOIN public.storage_digest_map_intents m USING (object_id)
        JOIN public.storage_digest_inventory_intents p USING (map_id)
        WHERE w.object_id IN (old_object, pg_temp.catalogue_id('76'), pg_temp.catalogue_id('79'), pg_temp.catalogue_id('7c'));
END $$;

-- Observe real first-insert contention from the independent fixture client.
CREATE FUNCTION pg_temp.wait_for_catalogue_lock(pid INTEGER) RETURNS VOID LANGUAGE plpgsql AS $$
DECLARE deadline CONSTANT TIMESTAMPTZ := clock_timestamp() + interval '3 seconds';
BEGIN
    WHILE cardinality(pg_blocking_pids(pid)) = 0 LOOP
        IF clock_timestamp() >= deadline THEN
            RAISE EXCEPTION 'competing writer failed to wait on occurrence lock';
        END IF;
        PERFORM pg_sleep(0.01);
    END LOOP;
END $$;
