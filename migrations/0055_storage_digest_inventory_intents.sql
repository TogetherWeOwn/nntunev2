SET search_path = public, pg_catalog;

-- Inactive inventory only. Keys derive from the immutable object intent and
-- map; array subscript i + 1 is page i. NULL pins describe an unversioned
-- shape, not proof that the provider profile is qualified or immutable.
CREATE TABLE IF NOT EXISTS public.storage_digest_inventory_intents (
    map_id UUID PRIMARY KEY REFERENCES public.storage_digest_map_intents (map_id) ON DELETE RESTRICT,
    media_version TEXT COLLATE "C",
    page_versions TEXT[] COLLATE "C" NOT NULL
);

CREATE OR REPLACE FUNCTION public.check_storage_digest_inventory_intent() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, public AS $$
DECLARE
    expected_pages INTEGER;
    page_version TEXT COLLATE "C";
BEGIN
    SELECT m.page_count INTO expected_pages
      FROM public.storage_digest_map_intents m WHERE m.map_id = NEW.map_id;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'missing digest map intent' USING ERRCODE = '23503';
    END IF;
    IF array_ndims(NEW.page_versions) IS DISTINCT FROM 1
       OR array_lower(NEW.page_versions, 1) IS DISTINCT FROM 1
       OR cardinality(NEW.page_versions) IS DISTINCT FROM expected_pages
       OR (SELECT count(*) FROM public.storage_digest_map_page_intents p
           WHERE p.map_id = NEW.map_id) <> expected_pages THEN
        RAISE EXCEPTION 'incomplete digest inventory' USING ERRCODE = '23514';
    END IF;
    -- The page primary key and checked range make this count consecutive.
    -- A version ID is opaque and only identifies bytes together with its key.
    IF NEW.media_version = 'null' OR octet_length(NEW.media_version) NOT BETWEEN 1 AND 1024 THEN
        RAISE EXCEPTION 'invalid media version pin' USING ERRCODE = '23514';
    END IF;
    FOREACH page_version IN ARRAY NEW.page_versions LOOP
        IF (page_version IS NULL) <> (NEW.media_version IS NULL)
           OR page_version = 'null' OR octet_length(page_version) NOT BETWEEN 1 AND 1024 THEN
            RAISE EXCEPTION 'invalid sidecar version pin' USING ERRCODE = '23514';
        END IF;
    END LOOP;
    RETURN NEW;
END $$;

DROP TRIGGER IF EXISTS storage_digest_inventory_intent_complete ON public.storage_digest_inventory_intents;
CREATE TRIGGER storage_digest_inventory_intent_complete BEFORE INSERT
    ON public.storage_digest_inventory_intents FOR EACH ROW
    EXECUTE FUNCTION public.check_storage_digest_inventory_intent();
DROP TRIGGER IF EXISTS storage_digest_inventory_intent_append_only ON public.storage_digest_inventory_intents;
CREATE TRIGGER storage_digest_inventory_intent_append_only BEFORE UPDATE OR DELETE OR TRUNCATE
    ON public.storage_digest_inventory_intents FOR EACH STATEMENT
    EXECUTE FUNCTION public.storage_object_intent_append_only();
REVOKE ALL ON public.storage_digest_inventory_intents FROM PUBLIC;
REVOKE ALL ON FUNCTION public.check_storage_digest_inventory_intent() FROM PUBLIC;
