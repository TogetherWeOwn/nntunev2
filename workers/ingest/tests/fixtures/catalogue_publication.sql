-- Rehearsal schema for the service-owned one-source-per-occurrence slot.
-- The object keys immutable locator, winner, map and receipt records.
CREATE TABLE public.catalogue_s3_sources (
    track_id UUID PRIMARY KEY REFERENCES public.tracks (id),
    source_job_id TEXT REFERENCES public.ingest_jobs (id) ON DELETE SET NULL,
    accepted_seq BIGINT NOT NULL,
    object_id UUID NOT NULL REFERENCES public.storage_s3_locator_intents (object_id)
        REFERENCES public.storage_validated_digest_winners (object_id)
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
DO $$
DECLARE
    old_object CONSTANT UUID := '73737373-7373-4073-8073-737373737373';
    new_object CONSTANT UUID := '76767676-7676-4076-8076-767676767676';
    generation CONSTANT UUID := '77777777-7777-4077-8077-777777777777';
    new_map CONSTANT UUID := '78787878-7878-4078-8078-787878787878';
    digest CONSTANT BYTEA := decode(repeat('fd', 32), 'hex');
    prior public.storage_object_intents%ROWTYPE;
    envelope public.storage_object_envelope_intents%ROWTYPE;
BEGIN
    SELECT * INTO STRICT prior FROM public.storage_object_intents WHERE object_id = old_object;
    SELECT * INTO STRICT envelope FROM public.storage_object_envelope_intents WHERE object_id = old_object;
    INSERT INTO public.storage_object_intents VALUES (
        new_object, generation, prior.environment_id, prior.library_id,
        prior.provider_configuration_version_id, prior.plaintext_bytes, prior.envelope_bytes, prior.header_tag);
    INSERT INTO public.storage_object_envelope_intents VALUES (
        new_object, envelope.root_version_id,
        overlay(envelope.wrapped_record placing uuid_send(new_object) || uuid_send(generation) from 58 for 32));
    INSERT INTO public.storage_plaintext_digest_claim_intents VALUES (
        new_object, prior.environment_id, prior.library_id, prior.plaintext_bytes, digest);
    INSERT INTO public.storage_digest_map_intents (map_id, object_id, environment_id, library_id, plaintext_bytes)
        VALUES (new_map, new_object, prior.environment_id, prior.library_id, prior.plaintext_bytes);
    INSERT INTO public.storage_digest_map_page_intents (map_id, chunk_count, page_index, expected_hash)
        SELECT new_map, chunk_count, page_index, expected_hash FROM public.storage_digest_map_page_intents
        WHERE map_id = (SELECT map_id FROM public.storage_digest_map_intents WHERE object_id = old_object);
    INSERT INTO public.storage_digest_inventory_intents VALUES (new_map, 'new-media-pin', ARRAY['new-page-pin']);
    INSERT INTO public.storage_validated_digest_winners
        (object_id, environment_id, library_id, plaintext_bytes, plaintext_sha256, root_version_id, retain_until)
        SELECT new_object, environment_id, library_id, plaintext_bytes, digest, root_version_id, retain_until
        FROM public.storage_validated_digest_winners WHERE object_id = old_object;
    INSERT INTO public.storage_s3_locator_intents
        SELECT w.object_id, w.root_version_id, 'test-bucket',
               'media/' || encode(uuid_send(i.object_id), 'hex') || '/' || encode(uuid_send(i.generation_id), 'hex'),
               p.media_version, NULL
        FROM public.storage_validated_digest_winners w
        JOIN public.storage_object_intents i USING (object_id)
        JOIN public.storage_digest_map_intents m USING (object_id)
        JOIN public.storage_digest_inventory_intents p USING (map_id)
        WHERE w.object_id IN (old_object, new_object);
END $$;
