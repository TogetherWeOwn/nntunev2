UPDATE ingest_originals AS original
SET accepted_audio_classifications_v1 = coalesce(
  original.accepted_audio_classifications_v1, $3)
FROM ingest_jobs AS job,
     (SELECT id FROM users WHERE id = $2 AND deleting_at IS NULL FOR SHARE) AS writer_fence
WHERE original.job_id = $1
  AND job.id = original.job_id
  AND job.user_id = $2
  AND job.user_id = writer_fence.id
  AND original.content_sha256 = $4
  AND job.content_sha256 = original.content_sha256
RETURNING original.accepted_audio_classifications_v1
