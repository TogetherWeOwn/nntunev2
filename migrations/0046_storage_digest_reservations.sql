SET search_path = public, pg_catalog;

-- Coordination only: a reservation is not validation, publication or reuse authority.
-- Rows persist after release so an old generation cannot become current again.
CREATE TABLE IF NOT EXISTS public.storage_digest_reservations (
    environment_id UUID NOT NULL,
    library_id UUID NOT NULL,
    plaintext_sha256 BYTEA NOT NULL,
    plaintext_bytes BIGINT NOT NULL,
    object_id UUID NOT NULL,
    generation BIGINT NOT NULL CHECK (generation > 0),
    expires_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (environment_id, library_id, plaintext_sha256, plaintext_bytes),
    FOREIGN KEY (object_id, environment_id, library_id, plaintext_bytes, plaintext_sha256)
        REFERENCES public.storage_plaintext_digest_claim_intents
            (object_id, environment_id, library_id, plaintext_bytes, plaintext_sha256)
        ON DELETE RESTRICT
);

DROP TRIGGER IF EXISTS storage_digest_reservations_no_delete ON public.storage_digest_reservations;
CREATE TRIGGER storage_digest_reservations_no_delete BEFORE DELETE OR TRUNCATE
    ON public.storage_digest_reservations FOR EACH STATEMENT
    EXECUTE FUNCTION public.storage_object_intent_append_only();

-- The caller must authorize the namespace separately. Only an existing assertion
-- can contend; a NULL result means no reservation, never a reusable hit.
CREATE OR REPLACE FUNCTION public.reserve_storage_digest(p_object UUID, p_seconds INTEGER)
RETURNS BIGINT LANGUAGE plpgsql AS $$
DECLARE
    result BIGINT;
BEGIN
    IF p_seconds IS NULL OR p_seconds NOT BETWEEN 1 AND 3600 THEN
        RAISE EXCEPTION 'invalid reservation duration' USING ERRCODE = '22023';
    END IF;
    INSERT INTO public.storage_digest_reservations AS r
        (environment_id, library_id, plaintext_sha256, plaintext_bytes, object_id, generation, expires_at)
    SELECT environment_id, library_id, plaintext_sha256, plaintext_bytes, object_id, 1,
           clock_timestamp() + make_interval(secs => p_seconds)
    FROM public.storage_plaintext_digest_claim_intents WHERE object_id = p_object
    ON CONFLICT (environment_id, library_id, plaintext_sha256, plaintext_bytes)
    DO UPDATE SET object_id = EXCLUDED.object_id, generation = r.generation + 1,
                  expires_at = clock_timestamp() + make_interval(secs => p_seconds)
    WHERE r.expires_at <= clock_timestamp()
    RETURNING generation INTO result;
    RETURN result;
END $$;

-- A stale or expired holder cannot release its successor's reservation.
CREATE OR REPLACE FUNCTION public.release_storage_digest(p_object UUID, p_generation BIGINT)
RETURNS BOOLEAN LANGUAGE plpgsql AS $$
BEGIN
    UPDATE public.storage_digest_reservations
    SET expires_at = clock_timestamp()
    WHERE object_id = p_object AND generation = p_generation AND expires_at > clock_timestamp();
    RETURN FOUND;
END $$;
