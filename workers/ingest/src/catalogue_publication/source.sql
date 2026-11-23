-- Read the locked winner's deadlines after any lock wait.
WITH locked AS MATERIALIZED (
    SELECT w.object_id, w.environment_id, w.library_id, w.retain_until, w.delete_until
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
), live AS MATERIALIZED (
    SELECT * FROM locked WHERE retain_until > clock_timestamp() AND delete_until <= clock_timestamp()
), written AS (
    INSERT INTO public.catalogue_s3_sources AS current (track_id, source_job_id, accepted_seq, object_id)
    SELECT $15::text::uuid, $16, $17, object_id FROM live
    ON CONFLICT (track_id) DO UPDATE
        SET source_job_id = EXCLUDED.source_job_id,
            accepted_seq = EXCLUDED.accepted_seq,
            object_id = EXCLUDED.object_id
    WHERE current.accepted_seq < EXCLUDED.accepted_seq AND EXISTS (
        SELECT FROM public.storage_validated_digest_winners prior, live
        WHERE prior.object_id = current.object_id
          AND (prior.environment_id, prior.library_id) = (live.environment_id, live.library_id)
    )
    RETURNING 1
)
SELECT 1 FROM written
UNION ALL
SELECT 1 FROM public.catalogue_s3_sources s JOIN live USING (object_id)
WHERE s.track_id = $15::text::uuid AND s.source_job_id = $16 AND s.accepted_seq = $17
