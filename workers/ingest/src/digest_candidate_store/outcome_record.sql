-- In the transaction holding the scoped session lock, after staging its winner.
UPDATE public.ingest_session_reservations
SET terminal_reason = 'completed', terminal_at = pg_catalog.clock_timestamp(),
    completed_map_id = $2::text::uuid, write_lease_id = NULL, write_lease_generation = NULL
WHERE id = $1::text::uuid AND terminal_reason IS NULL AND write_lease_id = $3::text::uuid
  AND pg_catalog.clock_timestamp() < LEAST(idle_deadline, absolute_deadline,
      pg_catalog.to_timestamp($4::bigint::double precision))
RETURNING 1;
