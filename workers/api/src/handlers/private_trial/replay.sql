-- A replay has already locked the live owner session and redeemed invite
-- and re-read the committed upload under its own scope fence.
SELECT s.track_id::text
FROM public.catalogue_s3_sources s
JOIN public.private_library_admissions a USING (track_id, accepted_seq, object_id)
JOIN public.ingest_jobs j ON j.id = s.source_job_id AND j.accepted_seq = s.accepted_seq
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
JOIN public.storage_digest_inventory_intents p ON p.map_id = m.map_id
WHERE a.invite_id = $1 AND a.recipient_id = $2
  AND s.source_job_id = $3 AND j.user_id = $4 AND j.status = 'done'
  AND w.environment_id = $5::text::uuid AND w.library_id = $6::text::uuid
  AND w.object_id = $7::text::uuid AND i.generation_id = $8::text::uuid
  AND m.map_id = $9::text::uuid AND w.root_version_id = $10::text::uuid
  AND i.provider_configuration_version_id = $11::text::uuid
  AND p.media_version = $12 AND l.s3_version_id = p.media_version AND p.page_versions = $13
  AND l.s3_bucket = $14 AND w.plaintext_sha256 = $15
  AND l.s3_key = 'media/' || encode(uuid_send(i.object_id), 'hex') || '/' ||
                encode(uuid_send(i.generation_id), 'hex')
  AND n.owner_id IS NULL AND NOT w.quarantined AND w.deleting_at IS NULL
  AND w.retain_until > clock_timestamp() AND w.delete_until <= clock_timestamp()
FOR SHARE OF s, a
