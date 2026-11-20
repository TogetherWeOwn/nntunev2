SET search_path = public, pg_catalog;

-- Inactive assertions only: neither a digest nor this row proves origin,
-- authorization, validation, reservation, publication, or reuse eligibility.
-- Multiple objects may claim the same identity; no canonical winner is chosen.
CREATE TABLE IF NOT EXISTS public.storage_plaintext_digest_claim_intents (
    object_id UUID PRIMARY KEY,
    environment_id UUID NOT NULL,
    library_id UUID NOT NULL,
    plaintext_bytes BIGINT NOT NULL CHECK (plaintext_bytes BETWEEN 1 AND 8589934592),
    plaintext_sha256 BYTEA NOT NULL CHECK (octet_length(plaintext_sha256) = 32),
    UNIQUE (object_id, environment_id, library_id, plaintext_bytes, plaintext_sha256),
    FOREIGN KEY (object_id, environment_id, library_id, plaintext_bytes)
        REFERENCES public.storage_object_intents (object_id, environment_id, library_id, plaintext_bytes) ON DELETE RESTRICT
);

DROP TRIGGER IF EXISTS storage_plaintext_digest_claim_intent_append_only ON public.storage_plaintext_digest_claim_intents;
CREATE TRIGGER storage_plaintext_digest_claim_intent_append_only BEFORE UPDATE OR DELETE OR TRUNCATE
    ON public.storage_plaintext_digest_claim_intents FOR EACH STATEMENT
    EXECUTE FUNCTION public.storage_object_intent_append_only();
