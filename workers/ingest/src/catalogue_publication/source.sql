-- The canonical track row and the candidate winner are already locked.
-- Check the storage clock after the waits. Replay performs no update.
WITH live AS MATERIALIZED (
    SELECT object_id, environment_id, library_id
    FROM public.storage_validated_digest_winners
    WHERE object_id = $4::text::uuid AND retain_until > clock_timestamp()
      AND delete_until <= clock_timestamp()
), written AS (
    INSERT INTO public.catalogue_s3_sources AS current (track_id, source_job_id, accepted_seq, object_id)
    SELECT $1::text::uuid, $2, $3, object_id FROM live
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
WHERE s.track_id = $1::text::uuid AND s.source_job_id = $2 AND s.accepted_seq = $3
