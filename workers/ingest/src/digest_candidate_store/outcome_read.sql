-- The exact session is already locked and its authorization has been rechecked.
SELECT s.completed_map_id::text, m.object_id::text, i.generation_id::text,
       i.provider_configuration_version_id::text, w.root_version_id::text, p.media_version, p.page_versions
FROM public.ingest_session_reservations s
LEFT JOIN public.storage_digest_inventory_intents p ON p.map_id = s.completed_map_id
LEFT JOIN public.storage_digest_map_intents m ON m.map_id = p.map_id AND m.library_id = s.library_id
LEFT JOIN public.storage_object_intents i ON i.object_id = m.object_id AND i.generation_id = s.checkpoint_generation
LEFT JOIN public.storage_validated_digest_winners w ON w.object_id = i.object_id
WHERE s.id = $1::text::uuid
  AND (s.terminal_reason IS NULL OR s.terminal_reason = 'completed')
  AND (s.completed_map_id IS NULL OR (p.media_version IS NOT NULL AND NOT w.quarantined AND w.deleting_at IS NULL))
  AND pg_catalog.clock_timestamp() < LEAST(s.absolute_deadline,
      CASE WHEN s.completed_map_id IS NULL THEN s.idle_deadline ELSE w.retain_until END,
      pg_catalog.to_timestamp($2::bigint::double precision));
