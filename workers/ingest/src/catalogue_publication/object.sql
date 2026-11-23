SELECT 1
FROM public.storage_validated_digest_winners w
JOIN public.storage_object_intents i USING (object_id, environment_id, library_id, plaintext_bytes)
JOIN public.storage_digest_map_intents m USING (object_id, environment_id, library_id, plaintext_bytes)
JOIN public.storage_digest_inventory_intents p USING (map_id)
JOIN public.storage_s3_locator_intents l USING (object_id, root_version_id)
JOIN public.storage_library_namespaces n ON (n.id, n.environment_id) = (w.library_id, w.environment_id)
WHERE w.object_id = $1::text::uuid
  AND w.environment_id = $2::text::uuid AND w.library_id = $3::text::uuid
  AND n.owner_id IS NULL
  AND i.generation_id = $4::text::uuid AND i.provider_configuration_version_id = $5::text::uuid
  AND w.plaintext_bytes = $6 AND w.plaintext_sha256 = $7
  AND m.map_id = $8::text::uuid AND w.root_version_id = $9::text::uuid
  AND l.s3_bucket = $10 AND l.s3_key = $11
  AND l.s3_version_id = $12 AND p.media_version = $12 AND p.page_versions = $13
  AND i.header_tag = $14 AND NOT w.quarantined AND w.deleting_at IS NULL
FOR UPDATE OF w
