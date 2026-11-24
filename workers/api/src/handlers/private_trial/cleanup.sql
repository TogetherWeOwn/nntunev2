SELECT encode(uuid_send(i.generation_id), 'hex') AS generation,
       encode(uuid_send(m.map_id), 'hex') AS map, w.root_version_id::text AS root,
       p.media_version, p.page_versions[1] AS page_version, w.deleting_at IS NOT NULL AS retired
FROM public.storage_validated_digest_winners w
JOIN public.storage_library_namespaces n ON (n.id, n.environment_id) = (w.library_id, w.environment_id)
JOIN public.storage_object_intents i ON i.object_id = w.object_id
JOIN public.storage_s3_locator_intents l
  ON (l.object_id, l.root_version_id) = (w.object_id, w.root_version_id)
JOIN public.storage_digest_map_intents m ON m.object_id = w.object_id
JOIN public.storage_digest_inventory_intents p ON p.map_id = m.map_id
JOIN public.ingest_session_reservations u
  ON (u.multipart_object_id, u.completed_map_id, u.library_id) = (w.object_id, m.map_id, w.library_id)
JOIN public.ingest_jobs j ON j.id = $6 AND j.user_id = u.principal_id
JOIN public.invite_codes v ON v.id = $8 AND v.issued_by = u.principal_id
JOIN public.catalogue_s3_sources s
  ON s.track_id = $4::text::uuid AND s.source_job_id = j.id AND s.accepted_seq = j.accepted_seq
 AND s.object_id = w.object_id
WHERE w.environment_id = $1::text::uuid AND w.library_id = $2::text::uuid AND w.object_id = $3::text::uuid
  AND u.id = $5::text::uuid AND u.principal_id = $7 AND u.terminal_reason = 'completed'
  AND u.idempotency_key = j.id AND j.status = 'done' AND n.owner_id IS NULL
  AND v.kind = 'admin' AND v.email LIKE '%@example.test' AND v.used
  AND w.plaintext_sha256 = $11 AND l.s3_bucket = $9 AND u.multipart_bucket = $9
  AND extract(epoch FROM clock_timestamp()) >= $10
  AND p.media_version = l.s3_version_id AND cardinality(p.page_versions) = 1
  AND NOT w.quarantined
  AND (w.deleting_at IS NOT NULL OR (
      greatest(w.retain_until, w.read_until, w.upload_until, w.delete_until) <= clock_timestamp()
      AND EXISTS (
          SELECT FROM public.private_library_admissions a
          WHERE (a.track_id, a.accepted_seq, a.object_id) = (s.track_id, s.accepted_seq, s.object_id)
            AND a.invite_id = v.id AND a.recipient_id = v.redeemed_by
      )
      AND NOT EXISTS (
          SELECT FROM public.private_library_admissions a
          WHERE (a.track_id = $4::text::uuid OR a.object_id = w.object_id)
            AND (a.track_id, a.object_id, a.invite_id) <> ($4::text::uuid, w.object_id, v.id)
      )
  ))
FOR UPDATE OF w
