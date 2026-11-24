-- Reuse the publisher fixture's environment and shared library. The caller
-- stages the verified candidate and inserts its locator in the same transaction.
CREATE FUNCTION pg_temp.prepare_catalogue_candidate(
    ids TEXT[], map TEXT, plaintext_bytes BIGINT, envelope_bytes BIGINT,
    header BYTEA, digest BYTEA, page_hash BYTEA, envelope BYTEA
) RETURNS VOID LANGUAGE plpgsql AS $$
DECLARE
    root UUID := ids[1]::UUID;
    environment UUID := ids[2]::UUID;
    library UUID := ids[3]::UUID;
    object_id UUID := ids[4]::UUID;
    generation UUID := ids[5]::UUID;
    provider UUID := ids[6]::UUID;
BEGIN
    INSERT INTO public.storage_object_intents
        VALUES (object_id, generation, environment, library, provider,
                plaintext_bytes, envelope_bytes, header);
    INSERT INTO public.storage_plaintext_digest_claim_intents
        VALUES (object_id, environment, library, plaintext_bytes, digest);
    PERFORM public.reserve_storage_digest(object_id, 3600);
    PERFORM public.store_storage_digest_candidate(environment, library, object_id,
        map::UUID, 1, ARRAY[page_hash]);
    INSERT INTO public.storage_object_envelope_intents VALUES (object_id, root, envelope);
END $$;
