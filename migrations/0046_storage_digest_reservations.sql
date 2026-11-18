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

-- Table writers are trusted to manage expiry; keys and fencing history are immutable.
CREATE OR REPLACE FUNCTION public.storage_digest_reservation_guard()
RETURNS TRIGGER LANGUAGE plpgsql SET search_path = pg_catalog, public AS $$
BEGIN
    IF (NEW.environment_id, NEW.library_id, NEW.plaintext_sha256, NEW.plaintext_bytes)
       IS DISTINCT FROM (OLD.environment_id, OLD.library_id, OLD.plaintext_sha256, OLD.plaintext_bytes)
       OR NEW.generation NOT IN (OLD.generation, OLD.generation + 1)
       OR (NEW.generation = OLD.generation AND NEW.object_id <> OLD.object_id) THEN
        RAISE EXCEPTION 'reservation identity or generation cannot be rewritten' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END $$;
DROP TRIGGER IF EXISTS storage_digest_reservation_guard ON public.storage_digest_reservations;
CREATE TRIGGER storage_digest_reservation_guard BEFORE UPDATE ON public.storage_digest_reservations
    FOR EACH ROW EXECUTE FUNCTION public.storage_digest_reservation_guard();

-- The caller must authorize the namespace separately. Only an existing assertion
-- can contend; a NULL result means no reservation, never a reusable hit.
CREATE OR REPLACE FUNCTION public.reserve_storage_digest(p_object UUID, p_seconds INTEGER)
RETURNS BIGINT LANGUAGE plpgsql SET search_path = pg_catalog, public AS $$
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
    -- An insert may have waited for a conflicting uncommitted insert to roll back.
    -- Start the lease after that wait, while this transaction still owns the row.
    IF result IS NOT NULL THEN
        UPDATE public.storage_digest_reservations
        SET expires_at = clock_timestamp() + make_interval(secs => p_seconds)
        WHERE object_id = p_object AND generation = result;
    END IF;
    RETURN result;
END $$;

-- A stale or expired holder cannot release its successor's reservation.
CREATE OR REPLACE FUNCTION public.release_storage_digest(p_object UUID, p_generation BIGINT)
RETURNS BOOLEAN LANGUAGE plpgsql SET search_path = pg_catalog, public AS $$
BEGIN
    UPDATE public.storage_digest_reservations
    SET expires_at = clock_timestamp()
    WHERE object_id = p_object AND generation = p_generation AND expires_at > clock_timestamp();
    RETURN FOUND;
END $$;
