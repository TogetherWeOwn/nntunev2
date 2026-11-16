SET search_path = public, pg_catalog;

-- Inactive identities only: insertion does not establish sealer origin, verification,
-- completeness, provider authority, or eligibility. No runtime reads these rows.
CREATE UNIQUE INDEX storage_object_intent_geometry_identity
    ON public.storage_object_intents (object_id, environment_id, library_id, plaintext_bytes);

CREATE TABLE public.storage_digest_map_intents (
    map_id UUID PRIMARY KEY CHECK (public.storage_is_uuid_v4(map_id)),
    object_id UUID NOT NULL UNIQUE,
    environment_id UUID NOT NULL,
    library_id UUID NOT NULL,
    plaintext_bytes BIGINT NOT NULL CHECK (plaintext_bytes BETWEEN 1 AND 8589934592),
    chunk_count INTEGER GENERATED ALWAYS AS (((plaintext_bytes + 65535) / 65536)::INTEGER) STORED,
    page_count INTEGER GENERATED ALWAYS AS (((plaintext_bytes + 16777215) / 16777216)::INTEGER) STORED,
    UNIQUE (map_id, chunk_count),
    FOREIGN KEY (object_id, environment_id, library_id, plaintext_bytes)
        REFERENCES public.storage_object_intents (object_id, environment_id, library_id, plaintext_bytes) ON DELETE RESTRICT
);

CREATE TABLE public.storage_digest_map_page_intents (
    map_id UUID NOT NULL,
    chunk_count INTEGER NOT NULL CHECK (chunk_count BETWEEN 1 AND 131072),
    page_index INTEGER NOT NULL CHECK (page_index >= 0 AND page_index < (chunk_count + 255) / 256),
    first_chunk INTEGER GENERATED ALWAYS AS (256 * page_index) STORED,
    entry_count INTEGER GENERATED ALWAYS AS (LEAST(256, chunk_count - 256 * page_index)) STORED,
    expected_hash BYTEA NOT NULL CHECK (octet_length(expected_hash) = 32),
    PRIMARY KEY (map_id, page_index),
    FOREIGN KEY (map_id, chunk_count) REFERENCES public.storage_digest_map_intents (map_id, chunk_count) ON DELETE RESTRICT
);

CREATE TRIGGER storage_digest_map_intent_append_only BEFORE UPDATE OR DELETE OR TRUNCATE
    ON public.storage_digest_map_intents FOR EACH STATEMENT
    EXECUTE FUNCTION public.storage_object_intent_append_only();
CREATE TRIGGER storage_digest_map_page_intent_append_only BEFORE UPDATE OR DELETE OR TRUNCATE
    ON public.storage_digest_map_page_intents FOR EACH STATEMENT
    EXECUTE FUNCTION public.storage_object_intent_append_only();
