-- Reuse the exact committed delivery claim. Wait out the final session write
-- deadline before touching S3. Never change checkpoint or write-lease state.
WITH locked AS MATERIALIZED (
    SELECT s.id, s.multipart_upload_id, c.claimed_until,
           'media/' || replace(o.object_id::text, '-', '') || '/' ||
                       replace(o.generation_id::text, '-', '') AS media_key
    FROM public.ingest_session_reservations AS s
    JOIN public.storage_library_namespaces AS l ON l.id = s.library_id
    JOIN public.ingest_session_cleanup_intents AS c ON c.session_id = s.id
    LEFT JOIN public.storage_object_intents AS o ON o.object_id = s.multipart_object_id
    WHERE l.environment_id = $1::text::uuid AND s.library_id = $2::text::uuid
      AND s.principal_id = $3 AND s.id = $4::text::uuid
      AND c.kind = 'terminal_reconcile' AND c.delivery_claim_id = $5::text::uuid
      AND c.enqueued_at IS NULL AND c.due_at = s.terminal_at
      AND s.terminal_reason IN ('aborted', 'expired') AND s.write_lease_id IS NULL
      AND (s.multipart_object_id IS NULL OR (
          s.multipart_bucket = $6 AND o.environment_id = l.environment_id AND o.library_id = l.id
          AND o.provider_configuration_version_id = $7::text::uuid
          AND s.idle_deadline <= clock_timestamp()
      ))
    FOR UPDATE OF c
), live AS MATERIALIZED (
    SELECT * FROM locked WHERE clock_timestamp() < claimed_until
), pinned AS (
    UPDATE public.ingest_session_reservations AS s
    SET multipart_upload_id = $8::text
    FROM live
    WHERE s.id = live.id AND live.media_key IS NOT NULL AND $8::text IS NOT NULL
      AND s.multipart_upload_id IS NULL AND clock_timestamp() < live.claimed_until
    RETURNING s.id
)
SELECT media_key, COALESCE($8, multipart_upload_id),
       floor(extract(epoch FROM claimed_until) * 1000)::bigint
FROM live WHERE $8::text IS NULL OR EXISTS (SELECT FROM pinned WHERE pinned.id = live.id);
