-- Synthetic eligibility for the hosted race only; there is no application publication writer.
INSERT INTO public.storage_digest_map_intents (map_id, object_id, environment_id, library_id, plaintext_bytes)
SELECT gen_random_uuid(), object_id, environment_id, library_id, plaintext_bytes FROM public.storage_object_intents WHERE object_id::text LIKE '21000000-%';
-- Four single-page winners have distinct pins for the lookup/delete races.
INSERT INTO public.storage_digest_map_page_intents (map_id, chunk_count, page_index, expected_hash)
SELECT map_id, chunk_count, 0, decode(repeat('cd',32),'hex') FROM public.storage_digest_map_intents
WHERE object_id::text LIKE '21000000-%' AND right(object_id::text,2) IN ('09','10','11','12');
INSERT INTO public.storage_digest_inventory_intents (map_id, media_version, page_versions)
SELECT map_id, 'media-' || right(object_id::text,2), ARRAY['page-' || right(object_id::text,2)]
FROM public.storage_digest_map_intents
WHERE object_id::text LIKE '21000000-%' AND right(object_id::text,2) IN ('09','10','11','12');
INSERT INTO public.storage_object_envelope_intents
SELECT object_id, '51000000-0000-4000-8000-000000000001', decode('4e4e544d454430310002','hex')
    || uuid_send('51000000-0000-4000-8000-000000000001') || uuid_send(environment_id) || uuid_send(library_id)
    || uuid_send(object_id) || uuid_send(generation_id) || uuid_send(provider_configuration_version_id)
    || int8send(plaintext_bytes) || int8send(envelope_bytes) || header_tag || decode(repeat('00',60),'hex')
FROM public.storage_object_intents WHERE object_id::text LIKE '21000000-%';
INSERT INTO public.storage_validated_digest_winners
    (object_id, environment_id, library_id, plaintext_bytes, plaintext_sha256, root_version_id, retain_until)
SELECT c.*, e.root_version_id, '-infinity' FROM public.storage_plaintext_digest_claim_intents c
JOIN public.storage_object_envelope_intents e USING (object_id)
WHERE c.object_id::text LIKE '21000000-%' AND right(c.object_id::text,2) IN ('01','09','10','11','12');
CREATE FUNCTION public.digest_ref_fixture(n int, deleting bool, fault text DEFAULT '', ref uuid DEFAULT NULL,
    deadline timestamptz DEFAULT clock_timestamp() + interval '1 minute') RETURNS bool LANGUAGE sql AS $$
SELECT CASE WHEN deleting THEN public.admit_storage_digest_delete(c.environment_id,c.library_id,c.plaintext_sha256,c.plaintext_bytes,c.object_id,i.generation_id,e.root_version_id)
ELSE public.attach_storage_digest_reference(
    CASE WHEN fault='environment' THEN gen_random_uuid() ELSE c.environment_id END,
    CASE WHEN fault='library' THEN gen_random_uuid() ELSE c.library_id END,
    CASE WHEN fault='digest' THEN decode(repeat('00',32),'hex') ELSE c.plaintext_sha256 END,
    c.plaintext_bytes + (fault='size')::int, c.object_id,
    CASE WHEN fault='generation' THEN gen_random_uuid() ELSE i.generation_id END,
    CASE WHEN fault='revision' THEN gen_random_uuid() ELSE e.root_version_id END,
    CASE WHEN fault='new reference' THEN gen_random_uuid() ELSE coalesce(ref,c.object_id) END,
    CASE WHEN fault='expired' THEN '-infinity'::timestamptz ELSE deadline END)
END FROM public.storage_plaintext_digest_claim_intents c JOIN public.storage_object_intents i USING (object_id)
JOIN public.storage_object_envelope_intents e USING (object_id) WHERE c.object_id = ('21000000-0000-4000-8000-' || lpad(n::text,12,'0'))::uuid $$;

CREATE FUNCTION public.digest_claim_statement(UUID, UUID, BYTEA, BIGINT, UUID, UUID, UUID, UUID, TIMESTAMPTZ, UUID)
RETURNS TABLE (object_id UUID, generation_id UUID, provider_configuration_version_id UUID,
               map_id UUID, root_version_id UUID, media_version TEXT, page_versions TEXT[])
LANGUAGE sql VOLATILE AS :'storage_digest_claim_lookup_sql';

CREATE FUNCTION public.digest_lookup_fixture(n int)
RETURNS TABLE (object_id UUID, media_version TEXT, page_versions TEXT[])
LANGUAGE sql VOLATILE AS $$
SELECT b.object_id, b.media_version, b.page_versions
FROM public.storage_plaintext_digest_claim_intents c
JOIN public.storage_object_intents i USING (object_id)
JOIN public.storage_object_envelope_intents e USING (object_id)
CROSS JOIN LATERAL public.digest_claim_statement(c.environment_id, c.library_id, c.plaintext_sha256,
    c.plaintext_bytes, c.object_id, i.generation_id, e.root_version_id, c.object_id,
    'infinity', i.provider_configuration_version_id) b
WHERE c.object_id = ('21000000-0000-4000-8000-' || lpad(n::text,12,'0'))::uuid
$$;

CREATE FUNCTION public.digest_read_statement(UUID, UUID, BYTEA, BIGINT, UUID, UUID, UUID, UUID, TIMESTAMPTZ, UUID, TIMESTAMPTZ)
RETURNS TABLE (object_id UUID, generation_id UUID, provider_configuration_version_id UUID,
               map_id UUID, root_version_id UUID, media_version TEXT, page_versions TEXT[], usable_until TIMESTAMPTZ)
LANGUAGE sql VOLATILE AS :'storage_digest_read_lookup_sql';

CREATE FUNCTION public.digest_read_lookup_fixture(n int, deadline timestamptz)
RETURNS TABLE (object_id UUID, media_version TEXT, page_versions TEXT[])
LANGUAGE sql VOLATILE AS $$
SELECT b.object_id, b.media_version, b.page_versions
FROM public.storage_plaintext_digest_claim_intents c
JOIN public.storage_object_intents i USING (object_id)
JOIN public.storage_object_envelope_intents e USING (object_id)
CROSS JOIN LATERAL public.digest_read_statement(c.environment_id, c.library_id, c.plaintext_sha256,
    c.plaintext_bytes, c.object_id, i.generation_id, e.root_version_id, c.object_id,
    deadline, i.provider_configuration_version_id, deadline) b
WHERE c.object_id = ('21000000-0000-4000-8000-' || lpad(n::text,12,'0'))::uuid
$$;

DO $$ DECLARE field text; BEGIN
    FOREACH field IN ARRAY ARRAY['environment','library','digest','size','generation','revision','expired'] LOOP
        IF public.digest_ref_fixture(1,false,field) IS DISTINCT FROM false THEN RAISE EXCEPTION 'attach accepted %', field; END IF;
    END LOOP;
    IF public.digest_ref_fixture(2,false) IS DISTINCT FROM false THEN RAISE EXCEPTION 'unvalidated attach'; END IF;
    FOREACH field IN ARRAY ARRAY['retain_until','read_until','upload_until','delete_until'] LOOP
        EXECUTE format('UPDATE public.storage_validated_digest_winners SET %I = ''infinity'' WHERE right(object_id::text,2) = ''01''', field);
        IF public.digest_ref_fixture(1,true) IS DISTINCT FROM false THEN RAISE EXCEPTION 'delete bypassed %', field; END IF;
        EXECUTE format('UPDATE public.storage_validated_digest_winners SET %I = ''-infinity'' WHERE right(object_id::text,2) = ''01''', field);
    END LOOP;
    INSERT INTO public.storage_validated_digest_winners SELECT e.object_id,w.environment_id,w.library_id,w.plaintext_bytes,w.plaintext_sha256,e.root_version_id,false,w.retain_until
    FROM public.storage_validated_digest_winners w, public.storage_object_envelope_intents e
    WHERE right(w.object_id::text,2)='01' AND e.object_id='21000000-0000-4000-8000-000000000002' ON CONFLICT DO NOTHING;
    IF FOUND THEN RAISE EXCEPTION 'duplicate scoped winner'; END IF;
    IF EXISTS (SELECT FROM public.storage_validated_digest_references) THEN RAISE EXCEPTION 'refusal wrote a reference'; END IF;
    IF public.digest_ref_fixture(1,false) IS DISTINCT FROM true THEN RAISE EXCEPTION 'valid attach refused'; END IF;
    IF public.digest_ref_fixture(1,false) IS DISTINCT FROM false THEN RAISE EXCEPTION 'duplicate attach'; END IF;
    UPDATE public.storage_validated_digest_winners SET quarantined=true WHERE right(object_id::text,2)='01';
    IF public.digest_ref_fixture(1,false,'new reference') IS DISTINCT FROM false THEN RAISE EXCEPTION 'quarantined attach'; END IF;
END $$;
