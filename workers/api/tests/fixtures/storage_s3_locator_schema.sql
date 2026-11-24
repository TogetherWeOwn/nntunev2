SET search_path = public, pg_catalog;

-- Inactive location only. The object intent supplies the provider configuration,
-- P, E and H; the referenced scheme-2 record is historical, not a serving or
-- rewrap selector. Bucket ownership and pin provenance remain trusted inputs.
CREATE TABLE IF NOT EXISTS public.storage_s3_locator_intents (
    object_id UUID PRIMARY KEY,
    root_version_id UUID NOT NULL,
    s3_bucket TEXT COLLATE "C" NOT NULL CHECK (
        octet_length(s3_bucket) BETWEEN 3 AND 63
        AND s3_bucket ~ '^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)*$'
        AND s3_bucket !~ '^[0-9]+(\.[0-9]+){3}$'
    ),
    s3_key TEXT COLLATE "C" NOT NULL,
    s3_version_id TEXT COLLATE "C",
    s3_etag TEXT COLLATE "C",
    FOREIGN KEY (object_id, root_version_id)
        REFERENCES public.storage_object_envelope_intents (object_id, root_version_id) ON DELETE RESTRICT,
    CHECK (num_nonnulls(s3_version_id, s3_etag) = 1),
    CHECK (s3_version_id IS NULL OR (
        octet_length(s3_version_id) BETWEEN 1 AND 1024
        AND s3_version_id <> 'null' AND s3_version_id ~ '^[!-~]+$'
    )),
    -- Strong, opaque entity-tag, including quotes; never a digest.
    CHECK (s3_etag IS NULL OR (
        octet_length(s3_etag) BETWEEN 3 AND 1026 AND s3_etag ~ '^"[!#-~]+"$'
    ))
);

CREATE OR REPLACE FUNCTION public.check_storage_s3_locator_intent() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, public AS $$
DECLARE
    generation UUID;
BEGIN
    SELECT i.generation_id INTO generation
      FROM public.storage_object_intents i WHERE i.object_id = NEW.object_id;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'missing storage object intent' USING ERRCODE = '23503';
    END IF;
    -- Since the key embeds the globally unique object ID, the primary key
    -- also prevents reusing the provider key under another configuration.
    IF NEW.s3_key IS DISTINCT FROM ('media/' || encode(uuid_send(NEW.object_id), 'hex')
                                   || '/' || encode(uuid_send(generation), 'hex')) COLLATE "C" THEN
        RAISE EXCEPTION 'storage locator key does not match object intent' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END $$;

DROP TRIGGER IF EXISTS storage_s3_locator_intent_context ON public.storage_s3_locator_intents;
CREATE TRIGGER storage_s3_locator_intent_context BEFORE INSERT
    ON public.storage_s3_locator_intents FOR EACH ROW
    EXECUTE FUNCTION public.check_storage_s3_locator_intent();
DROP TRIGGER IF EXISTS storage_s3_locator_intent_append_only ON public.storage_s3_locator_intents;
CREATE TRIGGER storage_s3_locator_intent_append_only BEFORE UPDATE OR DELETE OR TRUNCATE
    ON public.storage_s3_locator_intents FOR EACH STATEMENT
    EXECUTE FUNCTION public.storage_object_intent_append_only();
REVOKE ALL ON public.storage_s3_locator_intents FROM PUBLIC;
REVOKE ALL ON FUNCTION public.check_storage_s3_locator_intent() FROM PUBLIC;
