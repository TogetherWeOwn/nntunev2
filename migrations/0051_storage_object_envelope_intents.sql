SET search_path = public, pg_catalog;

-- Inactive scheme-2 record history. A root version is public context, not a
-- key or proof of authorized root lineage. No row selects a serving revision.
CREATE TABLE IF NOT EXISTS public.storage_object_envelope_intents (
    object_id UUID NOT NULL REFERENCES public.storage_object_intents (object_id) ON DELETE RESTRICT,
    root_version_id UUID NOT NULL CHECK (public.storage_is_uuid_v4(root_version_id)),
    wrapped_record BYTEA NOT NULL CHECK (octet_length(wrapped_record) = 230),
    PRIMARY KEY (object_id, root_version_id)
);

CREATE OR REPLACE FUNCTION public.check_storage_object_envelope_intent() RETURNS trigger
LANGUAGE plpgsql SET search_path = public, pg_catalog AS $$
DECLARE
    intent public.storage_object_intents%ROWTYPE;
BEGIN
    SELECT * INTO intent FROM public.storage_object_intents WHERE object_id = NEW.object_id;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'missing storage object intent' USING ERRCODE = '23503';
    END IF;
    -- Scheme-2 prefix and zero nonce; the opaque suffix is not authenticated
    -- here. The immutable object intent supplies context, never the record.
    IF substring(NEW.wrapped_record FROM 1 FOR 182) IS DISTINCT FROM (
        decode('4e4e544d454430310002', 'hex') || uuid_send(NEW.root_version_id)
        || uuid_send(intent.environment_id) || uuid_send(intent.library_id)
        || uuid_send(intent.object_id) || uuid_send(intent.generation_id)
        || uuid_send(intent.provider_configuration_version_id)
        || int8send(intent.plaintext_bytes) || int8send(intent.envelope_bytes)
        || intent.header_tag || decode(repeat('00', 12), 'hex')
    ) THEN
        RAISE EXCEPTION 'storage envelope intent context mismatch' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END $$;

DROP TRIGGER IF EXISTS storage_object_envelope_intent_context ON public.storage_object_envelope_intents;
CREATE TRIGGER storage_object_envelope_intent_context BEFORE INSERT
    ON public.storage_object_envelope_intents FOR EACH ROW
    EXECUTE FUNCTION public.check_storage_object_envelope_intent();
DROP TRIGGER IF EXISTS storage_object_envelope_intent_append_only ON public.storage_object_envelope_intents;
CREATE TRIGGER storage_object_envelope_intent_append_only BEFORE UPDATE OR DELETE OR TRUNCATE
    ON public.storage_object_envelope_intents FOR EACH STATEMENT
    EXECUTE FUNCTION public.storage_object_intent_append_only();
REVOKE ALL ON public.storage_object_envelope_intents FROM PUBLIC;
REVOKE ALL ON FUNCTION public.check_storage_object_envelope_intent() FROM PUBLIC;
