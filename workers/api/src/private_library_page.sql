SELECT t.id::text, t.mbid::text, t.release_id::text, t.title,
       t.duration_ms, t.track_no, t.medium_no
FROM public.users u
JOIN public.invite_codes i ON i.code_hash = u.invite_code_hash AND i.redeemed_by = u.id
JOIN public.private_library_admissions a ON (a.invite_id, a.recipient_id) = (i.id, u.id)
JOIN public.catalogue_s3_sources s USING (track_id, accepted_seq, object_id)
JOIN public.tracks t ON t.id = s.track_id
JOIN public.storage_validated_digest_winners w ON w.object_id = s.object_id
JOIN public.storage_library_namespaces n
  ON (n.id, n.environment_id) = (w.library_id, w.environment_id)
WHERE (SELECT allowed FROM access) AND u.id = $1 AND u.id <> $2 AND i.id = ANY($3)
  AND (w.environment_id, w.library_id) = ($4::text::uuid, $5::text::uuid)
  AND n.owner_id IS NULL AND extract(epoch FROM statement_timestamp()) < $6
  AND NOT w.quarantined AND w.deleting_at IS NULL
  AND w.retain_until > statement_timestamp() AND w.delete_until <= statement_timestamp()
ORDER BY a.accepted_seq DESC, a.track_id LIMIT $7
