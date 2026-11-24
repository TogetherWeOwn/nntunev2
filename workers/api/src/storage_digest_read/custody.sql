-- Read only the exact immutable records selected by a committed protected read.
SELECT i.header_tag, e.wrapped_record,
       ARRAY(SELECT p.expected_hash FROM public.storage_digest_map_page_intents p
             WHERE p.map_id = m.map_id ORDER BY p.page_index LIMIT 512) AS hashes
FROM public.storage_object_intents i
JOIN public.storage_object_envelope_intents e USING (object_id)
JOIN public.storage_digest_map_intents m USING (object_id)
JOIN public.storage_s3_locator_intents l ON (l.object_id, l.root_version_id) = (e.object_id, e.root_version_id)
WHERE i.object_id = $1::text::uuid AND e.root_version_id = $2::text::uuid
  AND m.map_id = $3::text::uuid
  AND l.s3_bucket = $4 AND l.s3_key = $5 AND l.s3_version_id = $6
