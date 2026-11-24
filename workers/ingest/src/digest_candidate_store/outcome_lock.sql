WITH locked AS MATERIALIZED (
    SELECT s.completed_map_id, s.idle_deadline, s.absolute_deadline
    FROM public.ingest_session_reservations s
    JOIN public.storage_library_namespaces l ON l.id = s.library_id
    WHERE current_setting('transaction_isolation') = 'read committed'
      AND l.environment_id = $1::text::uuid AND s.library_id = $2::text::uuid
      AND s.principal_id = $3 AND s.id = $4::text::uuid AND s.fingerprint = $5
      AND s.checkpoint_generation = $6::text::uuid AND s.checkpoint_revision = $7
      AND s.checkpoint_offset = $8
      AND (s.terminal_reason IS NULL OR s.terminal_reason = 'completed')
      AND ($9::text::uuid IS NULL OR (s.write_lease_id = $9::text::uuid
                                    AND s.write_lease_generation = $6::text::uuid))
    FOR UPDATE OF s
)
SELECT 1 FROM locked
WHERE pg_catalog.clock_timestamp() < LEAST(absolute_deadline,
    CASE WHEN completed_map_id IS NULL THEN idle_deadline END);
