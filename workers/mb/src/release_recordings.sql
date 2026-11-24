SELECT r.mbid::text AS mbid, t.release_mbid::text AS release_mbid, r.title,
       t.track_no AS track_no, r.duration_ms, t.medium_no AS medium_no
FROM mb_track t JOIN mb_recording r ON r.mbid = t.recording_mbid
WHERE t.release_mbid = $1::text::uuid
ORDER BY t.medium_no ASC, t.track_no ASC, r.mbid ASC LIMIT 1000
