SELECT 1
FROM public.storage_validated_digest_winners w
JOIN public.storage_object_intents i USING (object_id)
JOIN public.storage_s3_locator_intents l
  ON (l.object_id, l.root_version_id) = (w.object_id, w.root_version_id)
JOIN public.storage_digest_map_intents m ON m.object_id = w.object_id
JOIN public.storage_digest_inventory_intents p ON p.map_id = m.map_id
JOIN public.ingest_session_reservations u
  ON (u.multipart_object_id, u.completed_map_id, u.library_id) = (w.object_id, m.map_id, w.library_id)
JOIN public.invite_codes v ON v.id = $14 AND v.issued_by = u.principal_id
WHERE (w.environment_id, w.library_id, w.object_id) = ($1::text::uuid, $2::text::uuid, $3::text::uuid)
  AND i.generation_id = $4::text::uuid AND m.map_id = $5::text::uuid
  AND i.provider_configuration_version_id = $6::text::uuid AND w.root_version_id = $7::text::uuid
  AND p.media_version = $8 AND l.s3_version_id = $8 AND p.page_versions = ARRAY[$9::text]
  AND l.s3_bucket = $15 AND u.multipart_bucket = $15 AND w.plaintext_sha256 = $16
  AND u.id = $11::text::uuid AND u.idempotency_key = $12 AND u.principal_id = $13
  AND u.terminal_reason = 'completed' AND v.kind = 'admin' AND v.used AND v.email LIKE '%@example.test'
  AND (EXISTS (SELECT FROM public.catalogue_s3_sources s
               WHERE (s.track_id, s.object_id, s.source_job_id) = ($10::text::uuid, w.object_id, $12))
       OR (w.deleting_at IS NOT NULL AND NOT EXISTS
           (SELECT FROM public.catalogue_s3_sources s WHERE s.object_id = w.object_id)))
