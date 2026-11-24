-- Attach the selected publication through the production admission function.
SELECT public.attach_storage_digest_reference(
    w.environment_id, w.library_id, w.plaintext_sha256, w.plaintext_bytes,
    w.object_id, i.generation_id, w.root_version_id, s.track_id, $2
)
FROM public.catalogue_s3_sources s
JOIN public.storage_validated_digest_winners w USING (object_id)
JOIN public.storage_object_intents i ON i.object_id = w.object_id
WHERE s.track_id = $1::text::uuid
