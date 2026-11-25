-- Resolve one synthetic receipt persisted by the ingest regression.
SELECT w.*, i.generation_id, i.provider_configuration_version_id, i.header_tag,
       m.map_id, l.s3_bucket, l.s3_key, l.s3_version_id, p.page_versions,
       w.environment_id::text AS environment, w.library_id::text AS library
FROM public.storage_validated_digest_winners w
JOIN public.storage_object_intents i USING (object_id)
JOIN public.storage_digest_map_intents m USING (object_id)
JOIN public.storage_s3_locator_intents l USING (object_id)
JOIN public.storage_digest_inventory_intents p USING (map_id)
