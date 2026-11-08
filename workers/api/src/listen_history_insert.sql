INSERT INTO public.listen_events (user_id, playback_id, track_id, event, phase, recorded_at)
SELECT writer.id, $2, $3, $4, $5, $6
FROM (SELECT id FROM public.users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer
WHERE $5 = 0 OR EXISTS (
    SELECT 1 FROM public.listen_events AS started
    WHERE started.user_id = writer.id AND started.playback_id = $2
      AND started.phase = 0 AND started.track_id = $3
)
ON CONFLICT (user_id, playback_id, phase) DO UPDATE
SET event = listen_events.event
WHERE listen_events.track_id = EXCLUDED.track_id AND listen_events.event = EXCLUDED.event
RETURNING id
