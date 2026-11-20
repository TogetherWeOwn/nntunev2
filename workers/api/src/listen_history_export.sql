-- Resolve the anchor within the same owner. A cleared or unknown anchor
-- yields an empty page. The internal ordering key never leaves PostgreSQL.
SELECT playback_id, track_id, event, recorded_at
FROM public.listen_events
WHERE user_id = $1 AND ($2::text IS NULL OR id < (
    SELECT id FROM public.listen_events
    WHERE user_id = $1 AND playback_id = $2 AND phase = $3
))
ORDER BY id DESC LIMIT $4
