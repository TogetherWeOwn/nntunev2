SELECT l.job_id, l.release_mbid, l.recording_mbids, u.recording_mbid, u.recording_ord,
       (l.release_mbid IS NULL) AS matched_last, l.idempotency_key
FROM library l
LEFT JOIN LATERAL (SELECT value AS recording_mbid, ord AS recording_ord
       FROM unnest(l.recording_mbids) WITH ORDINALITY AS u2(value, ord)) AS u ON true
WHERE (l.user_id = $1 OR l.user_id IS NULL) AND coalesce(cardinality(l.recording_mbids), 0) > 0
UNION ALL
SELECT l.job_id, l.release_mbid, l.recording_mbids, NULL::text AS recording_mbid,
       0::bigint AS recording_ord, (l.release_mbid IS NULL) AS matched_last, l.idempotency_key
FROM library l
WHERE (l.user_id = $1 OR l.user_id IS NULL) AND coalesce(cardinality(l.recording_mbids), 0) = 0
ORDER BY matched_last ASC, job_id DESC, recording_ord ASC, idempotency_key ASC LIMIT $2
