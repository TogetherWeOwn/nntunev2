-- Client-reported history is not billing-meter evidence. Retain by default
-- until explicitly cleared or the account is erased; never age-drop receipts.
SET search_path = public, pg_catalog;

CREATE TABLE IF NOT EXISTS public.listen_events (
    id BIGSERIAL PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES public.users (id) ON DELETE CASCADE,
    playback_id TEXT NOT NULL CHECK (
        playback_id ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
    ),
    -- Opaque catalogue/demo reference, not a media URL or a meter track FK.
    track_id TEXT NOT NULL CHECK (octet_length(track_id) BETWEEN 1 AND 128),
    event TEXT NOT NULL CHECK (event IN ('start', 'complete', 'skip')),
    phase SMALLINT NOT NULL CHECK (
        (phase = 0 AND event = 'start') OR
        (phase = 1 AND event IN ('complete', 'skip'))
    ),
    recorded_at BIGINT NOT NULL CHECK (recorded_at >= 0),
    UNIQUE (user_id, playback_id, phase)
);
CREATE INDEX IF NOT EXISTS listen_events_owner_export
    ON public.listen_events (user_id, id DESC);
