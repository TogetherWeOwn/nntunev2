-- Select the canonical pair already published by the ingest producer regression.
SELECT s.track_id, s.track_id::text AS track, s.accepted_seq, w.*,
       i.generation_id, i.provider_configuration_version_id,
       encode(w.plaintext_sha256, 'hex') AS digest
FROM public.catalogue_s3_sources s
JOIN public.tracks t ON t.id = s.track_id
JOIN public.storage_validated_digest_winners w USING (object_id)
JOIN public.storage_object_intents i ON i.object_id = w.object_id
WHERE t.release_id = 'ab310000-0000-4000-8000-000000000000'
ORDER BY t.track_no
