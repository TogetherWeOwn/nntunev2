-- Seed an independent published track in the selected invite's storage scope.
CREATE FUNCTION pg_temp.seed_prior_trial_media(upload_object UUID, owner_id TEXT)
RETURNS TEXT LANGUAGE plpgsql SET search_path = pg_catalog, public AS $$
DECLARE
    prior_job CONSTANT TEXT := 'synthetic-prior-trial-publication';
    prior_track CONSTANT UUID := gen_random_uuid();
    prior_object CONSTANT UUID := gen_random_uuid();
    prior_generation CONSTANT UUID := gen_random_uuid();
    prior_map CONSTANT UUID := gen_random_uuid();
    prior_version CONSTANT TEXT := 'prior-fixture-media-version';
    prior_digest CONSTANT BYTEA := decode(repeat('ed', 32), 'hex');
    candidate public.storage_object_intents%ROWTYPE;
    root UUID;
BEGIN
    SELECT * INTO STRICT candidate FROM public.storage_object_intents WHERE object_id = upload_object;
    SELECT root_version_id INTO STRICT root FROM public.storage_validated_digest_winners WHERE object_id = upload_object;

    INSERT INTO public.storage_object_intents VALUES (
        prior_object, prior_generation, candidate.environment_id, candidate.library_id,
        candidate.provider_configuration_version_id, 177175, 177271, decode(repeat('00', 48), 'hex'));
    INSERT INTO public.storage_object_envelope_intents
    SELECT object_id, root, decode('4e4e544d454430310002', 'hex') || uuid_send(root)
        || uuid_send(environment_id) || uuid_send(library_id) || uuid_send(object_id)
        || uuid_send(generation_id) || uuid_send(provider_configuration_version_id)
        || int8send(plaintext_bytes) || int8send(envelope_bytes) || header_tag || decode(repeat('00', 60), 'hex')
    FROM public.storage_object_intents WHERE object_id = prior_object;
    INSERT INTO public.storage_plaintext_digest_claim_intents
    VALUES (prior_object, candidate.environment_id, candidate.library_id, 177175, prior_digest);
    INSERT INTO public.storage_digest_map_intents (map_id, object_id, environment_id, library_id, plaintext_bytes)
    VALUES (prior_map, prior_object, candidate.environment_id, candidate.library_id, 177175);
    INSERT INTO public.storage_digest_map_page_intents (map_id, chunk_count, page_index, expected_hash)
    VALUES (prior_map, 3, 0, decode(repeat('00', 32), 'hex'));
    INSERT INTO public.storage_digest_inventory_intents VALUES (prior_map, prior_version, ARRAY['prior-fixture-page-version']);
    INSERT INTO public.storage_validated_digest_winners
        (object_id, environment_id, library_id, plaintext_bytes, plaintext_sha256, root_version_id, retain_until)
    VALUES (prior_object, candidate.environment_id, candidate.library_id, 177175, prior_digest, root,
            clock_timestamp() + interval '1 hour');
    INSERT INTO public.storage_s3_locator_intents (object_id, root_version_id, s3_bucket, s3_key, s3_version_id)
    VALUES (prior_object, root, 'fixture-bucket',
            'media/' || encode(uuid_send(prior_object), 'hex') || '/' || encode(uuid_send(prior_generation), 'hex'), prior_version);

    INSERT INTO public.ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
    VALUES (prior_job, owner_id, prior_job, 1, 'done');
    INSERT INTO public.tracks (id, release_id, title, medium_no, track_no, duration_ms)
    VALUES (prior_track, gen_random_uuid(), 'Previously published fixture', 1, 1, 14000);
    INSERT INTO public.catalogue_s3_sources
    SELECT prior_track, id, accepted_seq, prior_object FROM public.ingest_jobs WHERE id = prior_job;
    INSERT INTO public.private_library_admissions
    SELECT 931, 'synthetic-trial-listener', track_id, accepted_seq, object_id
    FROM public.catalogue_s3_sources WHERE track_id = prior_track;
    INSERT INTO public.storage_validated_digest_references
    VALUES (candidate.environment_id, candidate.library_id, prior_track, prior_object);
    RETURN prior_track::text;
END $$;
