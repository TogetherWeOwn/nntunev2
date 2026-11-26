-- The selected track must still point to this job and its immutable object.
SELECT 1
FROM public.catalogue_s3_sources s
JOIN public.ingest_jobs j ON (j.id, j.accepted_seq) = (s.source_job_id, s.accepted_seq)
JOIN public.users u ON u.id = j.user_id
JOIN public.storage_validated_digest_winners w USING (object_id)
JOIN public.storage_library_namespaces n
  ON (n.id, n.environment_id) = (w.library_id, w.environment_id)
JOIN public.storage_s3_locator_intents l USING (object_id, root_version_id)
WHERE j.id = $1 AND j.user_id = $2 AND j.status = 'done' AND u.deleting_at IS NULL
  AND s.track_id = $3::text::uuid AND s.accepted_seq > 0
  AND w.environment_id = $4::text::uuid AND w.library_id = $5::text::uuid
  AND n.owner_id IS NULL
  AND (l.s3_bucket, l.s3_key, l.s3_version_id) = ($6, $7, $8)
  AND (w.plaintext_sha256, w.plaintext_bytes) = ($9, $10)
  AND NOT w.quarantined AND w.deleting_at IS NULL
  AND w.retain_until > clock_timestamp() AND w.delete_until <= clock_timestamp()
