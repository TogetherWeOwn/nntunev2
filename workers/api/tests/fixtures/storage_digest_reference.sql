-- Synthetic eligibility for the hosted race only; there is no application publication writer.
INSERT INTO public.storage_digest_map_intents (map_id, object_id, environment_id, library_id, plaintext_bytes)
SELECT gen_random_uuid(), object_id, environment_id, library_id, plaintext_bytes FROM public.storage_object_intents WHERE object_id::text LIKE '21000000-%';
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
