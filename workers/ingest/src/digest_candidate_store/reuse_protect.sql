-- Both rows remain locked. Clip the protection to every live deadline and
-- preserve any longer protection from another admitted reuse.
WITH expiry AS MATERIALIZED (
    SELECT w.object_id, LEAST(s.absolute_deadline, w.retain_until,
           pg_catalog.to_timestamp($3::bigint::double precision),
           pg_catalog.clock_timestamp() + interval '30 seconds') AS usable_until
    FROM public.ingest_session_reservations s, public.storage_validated_digest_winners w
    WHERE s.id = $1::text::uuid AND w.object_id = $2::text::uuid
      AND w.delete_until <= pg_catalog.clock_timestamp()
)
UPDATE public.storage_validated_digest_winners w
SET upload_until = greatest(w.upload_until, e.usable_until)
FROM expiry e
WHERE w.object_id = e.object_id AND pg_catalog.clock_timestamp() < e.usable_until
RETURNING e.usable_until;
