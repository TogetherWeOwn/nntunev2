SET search_path = public, pg_catalog;

-- Inactive, append-only sealing intents. A row is identity and geometry only: it
-- is not validation, key custody, a receipt, a digest map, or publication/reuse
-- authority, and it has no status. Provider-configuration IDs are trusted-config
-- values with no registry yet, so they are format-checked but not referenced.
CREATE OR REPLACE FUNCTION public.storage_is_uuid_v4(v UUID) RETURNS BOOLEAN
LANGUAGE sql IMMUTABLE STRICT SET search_path = public, pg_catalog AS $$
    SELECT v::text ~ '^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$'
$$;

CREATE UNIQUE INDEX IF NOT EXISTS storage_library_scope_identity
    ON public.storage_library_namespaces (id, environment_id);

CREATE TABLE IF NOT EXISTS public.storage_object_intents (
    object_id UUID PRIMARY KEY CHECK (public.storage_is_uuid_v4(object_id)),
    generation_id UUID NOT NULL UNIQUE CHECK (public.storage_is_uuid_v4(generation_id) AND generation_id <> object_id),
    environment_id UUID NOT NULL,
    library_id UUID NOT NULL,
    provider_configuration_version_id UUID NOT NULL CHECK (public.storage_is_uuid_v4(provider_configuration_version_id)),
    plaintext_bytes BIGINT NOT NULL CHECK (plaintext_bytes BETWEEN 1 AND 8589934592),
    envelope_bytes BIGINT NOT NULL,
    header_tag BYTEA NOT NULL CHECK (octet_length(header_tag) = 48),
    -- E = 48 + P + 16 * ceil(P / 65536), with P >= 1.
    CHECK (envelope_bytes = 48 + plaintext_bytes + 16 * ((plaintext_bytes + 65535) / 65536)),
    UNIQUE (object_id, environment_id, library_id),
    FOREIGN KEY (library_id, environment_id)
        REFERENCES public.storage_library_namespaces (id, environment_id) ON DELETE RESTRICT
);

CREATE OR REPLACE FUNCTION public.storage_object_intent_append_only() RETURNS trigger
LANGUAGE plpgsql SET search_path = public, pg_catalog AS $$
BEGIN
    RAISE EXCEPTION 'storage object intents are append-only' USING ERRCODE = '23514';
END $$;

DROP TRIGGER IF EXISTS storage_object_intent_append_only ON public.storage_object_intents;
CREATE TRIGGER storage_object_intent_append_only BEFORE UPDATE OR DELETE OR TRUNCATE
    ON public.storage_object_intents FOR EACH STATEMENT
    EXECUTE FUNCTION public.storage_object_intent_append_only();
