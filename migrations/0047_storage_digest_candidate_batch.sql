SET search_path = public, pg_catalog;

-- Inactive candidate rows only. The caller must authorize the namespace and
-- supply sealer-origin evidence; this function cannot establish either fact.
-- No result is a receipt, verified enrollment, publication, or reuse authority.
CREATE OR REPLACE FUNCTION public.store_storage_digest_candidate(
    p_environment UUID, p_library UUID, p_object UUID, p_map UUID,
    p_generation BIGINT, p_hashes BYTEA[]
) RETURNS VOID LANGUAGE plpgsql SET search_path = pg_catalog, public AS $$
DECLARE
    reservation public.storage_digest_reservations%ROWTYPE;
    chunks INTEGER;
    pages INTEGER;
BEGIN
    SELECT * INTO reservation FROM public.storage_digest_reservations
    WHERE environment_id = p_environment AND library_id = p_library
      AND object_id = p_object AND generation = p_generation
    FOR UPDATE;
    IF NOT FOUND OR reservation.expires_at <= clock_timestamp() THEN
        RAISE EXCEPTION 'inactive digest reservation' USING ERRCODE = '23514';
    END IF;
    chunks := ((reservation.plaintext_bytes + 65535) / 65536)::INTEGER;
    pages := (chunks + 255) / 256;
    IF p_hashes IS NULL OR array_ndims(p_hashes) <> 1 OR array_lower(p_hashes, 1) <> 1
       OR cardinality(p_hashes) <> pages OR EXISTS (
           SELECT FROM unnest(p_hashes) AS h WHERE h IS NULL OR octet_length(h) <> 32
       ) THEN
        RAISE EXCEPTION 'invalid digest candidate pages' USING ERRCODE = '23514';
    END IF;
    INSERT INTO public.storage_digest_map_intents
        (map_id, object_id, environment_id, library_id, plaintext_bytes)
    VALUES (p_map, p_object, p_environment, p_library, reservation.plaintext_bytes);
    INSERT INTO public.storage_digest_map_page_intents (map_id, chunk_count, page_index, expected_hash)
    SELECT p_map, chunks, (ordinality - 1)::INTEGER, hash
    FROM unnest(p_hashes) WITH ORDINALITY AS entries(hash, ordinality);
    IF reservation.expires_at <= clock_timestamp() THEN
        RAISE EXCEPTION 'digest reservation expired' USING ERRCODE = '23514';
    END IF;
END $$;
REVOKE ALL ON FUNCTION public.store_storage_digest_candidate(UUID, UUID, UUID, UUID, BIGINT, BYTEA[]) FROM PUBLIC;
