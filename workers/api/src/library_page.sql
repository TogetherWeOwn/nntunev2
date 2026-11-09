WITH library_rows AS (
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
)
SELECT * FROM library_rows
WHERE $3::boolean IS NULL
   OR matched_last > $3
   OR (matched_last = $3 AND job_id COLLATE "C" < $4::text COLLATE "C")
   OR (matched_last = $3 AND job_id COLLATE "C" = $4::text COLLATE "C" AND recording_ord > $5::bigint)
   OR (matched_last = $3 AND job_id COLLATE "C" = $4::text COLLATE "C" AND recording_ord = $5::bigint
       AND idempotency_key COLLATE "C" > $6::text COLLATE "C")
ORDER BY matched_last ASC, job_id COLLATE "C" DESC, recording_ord ASC, idempotency_key COLLATE "C" ASC LIMIT $2
