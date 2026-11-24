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

-- Fixture-only shape of the ADR 0002 Library dependency, not migrations.
CREATE TABLE public.libraries (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id text NOT NULL REFERENCES public.users(id) ON DELETE RESTRICT,
    name text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    deleting_at timestamptz
);
CREATE TABLE public.library_tracks (
    library_id uuid NOT NULL REFERENCES public.libraries(id) ON DELETE RESTRICT,
    track_id uuid NOT NULL REFERENCES public.tracks(id) ON DELETE RESTRICT,
    added_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (library_id, track_id)
);
