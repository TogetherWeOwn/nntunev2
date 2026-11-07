INSERT INTO library (job_id, user_id, idempotency_key, release_mbid, recording_mbids)
SELECT $1, NULL, $2, $3, $4
FROM (SELECT id FROM ingest_jobs WHERE id = $1 FOR SHARE) AS writer_fence
ON CONFLICT (idempotency_key) DO UPDATE SET
job_id = EXCLUDED.job_id,
user_id = EXCLUDED.user_id,
release_mbid = EXCLUDED.release_mbid,
recording_mbids = EXCLUDED.recording_mbids
WHERE library.user_id IS NULL
