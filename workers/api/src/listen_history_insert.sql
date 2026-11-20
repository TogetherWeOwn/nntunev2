-- The owner lock serializes first admission with grant acquisition. The
-- UUID and the grant's base64url play id are two encodings of the same bytes.
WITH play AS (
    SELECT translate(encode(uuid_send($2::uuid), 'base64'), '+/=', '-_') AS id
)
INSERT INTO public.listen_events (user_id, playback_id, track_id, event, phase, recorded_at)
SELECT $1, $2, $3, $4, $5, $6 FROM play
WHERE NOT EXISTS (
    SELECT 1 FROM public.private_plays AS private
    WHERE private.user_id = $1 AND private.play_key = 'play:' || $1 || ':' || $3 || ':' || play.id
)
AND (
    EXISTS (
        SELECT 1 FROM public.listen_events AS started
        WHERE started.user_id = $1 AND started.playback_id = $2
          AND started.phase = 0 AND started.track_id = $3
    ) OR ($5 = 0 AND EXISTS (
        SELECT 1 FROM public.playback_leases AS lease
        WHERE lease.user_id = $1 AND lease.play_id = play.id
          AND lease.track_id = $3 AND lease.expires_at > $6
    ))
)
ON CONFLICT (user_id, playback_id, phase) DO UPDATE
SET event = listen_events.event
WHERE listen_events.track_id = EXCLUDED.track_id AND listen_events.event = EXCLUDED.event
RETURNING 1
