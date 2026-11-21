-- Unmounted store statement. Caller must independently authorize this exact
-- (scope, object, reference), use a write transaction at READ COMMITTED, and
-- withhold the binding until commit is confirmed. A digest is not authority.
-- $1..$9 match attach_storage_digest_reference; $10 is the expected provider
-- configuration. Only version-pinned inventory is eligible in this slice.
WITH binding AS MATERIALIZED (
    SELECT i.object_id, i.environment_id, i.library_id, i.plaintext_bytes,
           i.generation_id, i.provider_configuration_version_id,
           m.map_id, inv.media_version, inv.page_versions
    FROM public.storage_object_intents i
    JOIN public.storage_digest_map_intents m USING (object_id)
    JOIN public.storage_digest_inventory_intents inv USING (map_id)
    WHERE i.environment_id = $1 AND i.library_id = $2 AND i.plaintext_bytes = $4
      AND i.object_id = $5 AND i.generation_id = $6
      AND i.provider_configuration_version_id = $10 AND inv.media_version IS NOT NULL
)
SELECT b.object_id, b.generation_id, b.provider_configuration_version_id,
       b.map_id, $7::uuid AS root_version_id, b.media_version, b.page_versions
FROM binding b
WHERE public.attach_storage_digest_reference(
    b.environment_id, b.library_id, $3, b.plaintext_bytes,
    b.object_id, b.generation_id, $7, $8, $9
);
