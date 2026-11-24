-- The exact scoped session and final checkpoint are already locked. Resolve
-- every pin from its committed map, without searching by a requested digest.
WITH candidate AS MATERIALIZED (
    SELECT m.map_id, i.object_id, i.environment_id, i.library_id, i.plaintext_bytes,
           i.generation_id, i.provider_configuration_version_id,
           w.plaintext_sha256, w.root_version_id, p.media_version, p.page_versions
    FROM public.ingest_session_reservations s
    JOIN public.storage_digest_inventory_intents p ON p.map_id = s.completed_map_id
    JOIN public.storage_digest_map_intents m ON m.map_id = p.map_id AND m.library_id = s.library_id
    JOIN public.storage_object_intents i
      ON (i.object_id, i.environment_id, i.library_id, i.plaintext_bytes) =
         (m.object_id, m.environment_id, m.library_id, m.plaintext_bytes)
    JOIN public.storage_validated_digest_winners w
      ON (w.object_id, w.environment_id, w.library_id, w.plaintext_bytes) =
         (i.object_id, i.environment_id, i.library_id, i.plaintext_bytes)
    WHERE s.id = $1::text::uuid AND s.terminal_reason = 'completed'
      AND i.generation_id = s.checkpoint_generation AND i.plaintext_bytes = s.checkpoint_offset
      AND p.media_version IS NOT NULL
)
SELECT c.map_id::text, c.object_id::text, c.generation_id::text,
       c.provider_configuration_version_id::text, c.root_version_id::text,
       c.media_version, c.page_versions
FROM candidate c
CROSS JOIN LATERAL public.lock_storage_digest_winner(
    c.environment_id, c.library_id, c.plaintext_sha256, c.plaintext_bytes,
    c.object_id, c.generation_id, c.root_version_id) w
WHERE w.object_id IS NOT NULL;
