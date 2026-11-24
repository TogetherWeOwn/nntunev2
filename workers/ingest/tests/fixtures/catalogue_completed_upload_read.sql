SELECT p.map_id::text, m.object_id::text, i.generation_id::text,
       i.provider_configuration_version_id::text, w.root_version_id::text,
       p.media_version, p.page_versions
FROM public.test_committed_uploads s
JOIN public.storage_digest_inventory_intents p USING (map_id)
JOIN public.storage_digest_map_intents m USING (map_id)
JOIN public.storage_object_intents i USING (object_id)
JOIN public.storage_validated_digest_winners w USING (object_id)
WHERE s.scope = $1;
