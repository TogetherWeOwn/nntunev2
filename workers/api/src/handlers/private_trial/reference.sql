SELECT CASE WHEN EXISTS (
    SELECT FROM public.storage_validated_digest_references r
    WHERE (r.environment_id, r.library_id, r.reference_id, r.object_id)
        = (w.environment_id, w.library_id, s.track_id, w.object_id)
) THEN true ELSE public.attach_storage_digest_reference(
    w.environment_id, w.library_id, w.plaintext_sha256, w.plaintext_bytes,
    w.object_id, i.generation_id, w.root_version_id, s.track_id, to_timestamp($2::bigint)
) END
FROM public.catalogue_s3_sources s
JOIN public.storage_validated_digest_winners w USING (object_id)
JOIN public.storage_object_intents i USING (object_id)
WHERE s.track_id = $1::text::uuid
