-- Join the immutable sealer intent and lock the same session row used for
-- progress, terminal transitions and completion. Check time after lock wait.
WITH locked AS MATERIALIZED (
    SELECT s.multipart_object_id IS NOT NULL AS claimed, s.multipart_upload_id,
           LEAST(s.idle_deadline, s.absolute_deadline) AS deadline
    FROM public.ingest_session_reservations s
    JOIN public.storage_object_intents i ON i.environment_id = $1::text::uuid
      AND i.library_id = s.library_id AND i.object_id = $10::text::uuid
      AND i.generation_id = $6::text::uuid AND i.provider_configuration_version_id = $11::text::uuid
      AND i.plaintext_bytes = $13 AND i.header_tag = $14
    WHERE s.library_id = $2::text::uuid AND s.principal_id = $3 AND s.id = $4::text::uuid
      AND s.fingerprint = $5 AND s.terminal_reason IS NULL
      AND s.write_lease_generation = i.generation_id AND s.write_lease_id = $9::text::uuid
      AND s.checkpoint_revision = $7 AND s.checkpoint_offset = $8 AND s.checkpoint_offset <= i.plaintext_bytes
      AND ((s.multipart_object_id IS NULL AND s.checkpoint_revision = 0)
           OR (s.multipart_object_id = i.object_id AND s.multipart_bucket = $12))
    FOR UPDATE OF s
)
SELECT claimed, multipart_upload_id, floor(extract(epoch FROM deadline) * 1000)::bigint
FROM locked WHERE clock_timestamp() < deadline AND isfinite(deadline);
