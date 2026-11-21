-- Rehearsal only. Do not add to the migration manifest before the atomic cutover.
ALTER TABLE public.playlists ADD COLUMN deleting_at timestamptz;
CREATE TABLE public.playlist_tracks (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    playlist_id bigint NOT NULL REFERENCES public.playlists(id) ON DELETE RESTRICT,
    track_id uuid NOT NULL REFERENCES public.tracks(id) ON DELETE RESTRICT,
    position integer NOT NULL CHECK (position >= 0),
    is_legacy boolean NOT NULL DEFAULT false,
    UNIQUE (playlist_id, position) DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX ON public.playlist_tracks (track_id);
CREATE INDEX ON public.playlist_tracks (playlist_id, track_id);

-- Require an explicit trusted mapping even when the legacy ref resembles a canonical ID.
INSERT INTO public.playlist_tracks (playlist_id, track_id, position, is_legacy)
SELECT p.id, r.track_id, r.position, true
FROM public.playlists p
JOIN pg_temp.legacy_resolution r ON r.playlist_id = p.id
WHERE p.items ->> r.position = r.legacy_ref;
