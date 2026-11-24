-- Only the canonical current source and its exact attached reference qualify.
SELECT s.track_id, w.object_id, i.generation_id, w.root_version_id,
       w.plaintext_sha256, w.plaintext_bytes, l.s3_key, t.duration_ms, encode(w.plaintext_sha256, 'hex') AS digest
FROM public.catalogue_s3_sources s
JOIN public.tracks t ON t.id = s.track_id
JOIN public.storage_validated_digest_winners w USING (object_id)
JOIN public.storage_validated_digest_references r
  ON (r.environment_id, r.library_id, r.reference_id, r.object_id) =
     (w.environment_id, w.library_id, s.track_id, w.object_id)
JOIN public.storage_library_namespaces n
  ON (n.id, n.environment_id) = (w.library_id, w.environment_id)
JOIN public.storage_object_intents i ON i.object_id = w.object_id
JOIN public.storage_s3_locator_intents l
  ON (l.object_id, l.root_version_id) = (w.object_id, w.root_version_id)
JOIN public.storage_digest_map_intents m ON m.object_id = w.object_id
JOIN public.storage_digest_inventory_intents inv USING (map_id)
WHERE s.track_id = $1::text::uuid
  AND w.environment_id = $2::text::uuid AND w.library_id = $3::text::uuid
  AND n.owner_id IS NULL AND s.accepted_seq = $4
  AND i.provider_configuration_version_id = $5::text::uuid
  AND l.s3_bucket = $6 AND l.s3_version_id = $7 AND inv.media_version = $7
  AND l.s3_key = 'media/' || encode(uuid_send(i.object_id), 'hex') || '/' ||
                encode(uuid_send(i.generation_id), 'hex')
  AND ($8 IS NULL OR w.plaintext_sha256 = decode($8, 'hex'))
  AND NOT w.quarantined AND w.deleting_at IS NULL AND w.delete_until <= clock_timestamp()
