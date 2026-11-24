-- Unmounted playback store statement. Independently authorize this existing
-- reference to this exact object before calling. Use READ COMMITTED; release
-- no binding before confirmed commit, and stop reads by the returned deadline.
-- $1..$10 are the claim lookup inputs; $11 is the requested read deadline.
WITH binding AS MATERIALIZED (
    SELECT i.object_id, i.environment_id, i.library_id, i.generation_id,
           i.provider_configuration_version_id, m.map_id, w.root_version_id,
           inv.media_version, inv.page_versions, w.delete_until
    FROM public.storage_object_intents i
    JOIN public.storage_digest_map_intents m USING (object_id)
    JOIN public.storage_digest_inventory_intents inv USING (map_id)
    CROSS JOIN LATERAL public.lock_storage_digest_winner(
        i.environment_id, i.library_id, $3, i.plaintext_bytes,
        i.object_id, i.generation_id, $7::text::uuid) w
    WHERE i.environment_id = $1::text::uuid AND i.library_id = $2::text::uuid AND i.plaintext_bytes = $4
      AND i.object_id = $5::text::uuid AND i.generation_id = $6::text::uuid
      AND i.provider_configuration_version_id = $10::text::uuid AND inv.media_version IS NOT NULL
      AND w.object_id IS NOT NULL
), referenced AS MATERIALIZED (
    -- Lock the reference after the winner; a snapshot alone could see a
    -- reference that was removed while waiting for the winner lock.
    SELECT b.* FROM binding b
    JOIN public.storage_validated_digest_references r
      ON (r.environment_id, r.library_id, r.reference_id, r.object_id) =
         (b.environment_id, b.library_id, $8::text::uuid, b.object_id)
    FOR UPDATE OF r
)
UPDATE public.storage_validated_digest_winners w
SET read_until = greatest(w.read_until, $11::timestamptz)
FROM referenced b
WHERE w.object_id = b.object_id AND b.delete_until <= clock_timestamp()
  AND isfinite($9::timestamptz) AND isfinite($11::timestamptz)
  AND $11 <= $9 AND $11 > clock_timestamp()
RETURNING b.object_id, b.generation_id, b.provider_configuration_version_id,
          b.map_id, b.root_version_id, b.media_version, b.page_versions, $11 AS usable_until;
