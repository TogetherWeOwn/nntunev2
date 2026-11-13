-- Additive mirror schema for CC0 genre identifiers and curated roll-ups.
-- This migration does not enable ingestion or schedule refreshes.
SET search_path = public, pg_catalog;
BEGIN;

CREATE TABLE IF NOT EXISTS public.genre (
    qid TEXT PRIMARY KEY CHECK (qid ~ '^Q[1-9][0-9]{0,19}$'),
    label TEXT CHECK (octet_length(label) <= 4096)
);
CREATE TABLE IF NOT EXISTS public.genre_root (
    root_id TEXT PRIMARY KEY CHECK (root_id ~ '^[a-z0-9_]{1,32}$'),
    display_name TEXT NOT NULL,
    sort_order INTEGER NOT NULL CHECK (sort_order > 0),
    retired BOOLEAN NOT NULL DEFAULT false
);
CREATE TABLE IF NOT EXISTS public.artist_genre (
    artist_mbid UUID NOT NULL REFERENCES public.mb_artist(mbid) ON DELETE CASCADE,
    genre_qid TEXT NOT NULL REFERENCES public.genre(qid),
    source TEXT NOT NULL CHECK (source IN ('wikidata', 'file_tag', 'manual')),
    PRIMARY KEY (artist_mbid, genre_qid, source)
);
CREATE INDEX IF NOT EXISTS artist_genre_genre_idx ON public.artist_genre(genre_qid, artist_mbid);
CREATE TABLE IF NOT EXISTS public.genre_rollup (
    genre_qid TEXT NOT NULL REFERENCES public.genre(qid),
    root_id TEXT NOT NULL REFERENCES public.genre_root(root_id),
    PRIMARY KEY (genre_qid, root_id)
);
CREATE TABLE IF NOT EXISTS public.genre_override (
    genre_qid TEXT NOT NULL CHECK (genre_qid ~ '^Q[1-9][0-9]{0,19}$'),
    action TEXT NOT NULL CHECK (action IN ('add', 'remove', 'replace', 'exclude')),
    roots TEXT[] NOT NULL,
    reason TEXT NOT NULL CHECK (length(reason) BETWEEN 1 AND 200),
    PRIMARY KEY (genre_qid, action)
);
CREATE TABLE IF NOT EXISTS public.genre_snapshot (
    singleton BOOLEAN PRIMARY KEY CHECK (singleton),
    identity JSONB NOT NULL,
    config JSONB NOT NULL
);

-- An unresolved manual assignment still suppresses lower-priority assignments.
CREATE OR REPLACE VIEW public.artist_genre_root AS
SELECT DISTINCT a.artist_mbid, r.root_id
FROM public.artist_genre a
JOIN public.genre_rollup r ON r.genre_qid = a.genre_qid
JOIN public.genre_root g ON g.root_id = r.root_id AND NOT g.retired
WHERE a.source = 'manual' OR NOT EXISTS (
    SELECT 1 FROM public.artist_genre m
    WHERE m.artist_mbid = a.artist_mbid AND m.source = 'manual'
);

COMMIT;
