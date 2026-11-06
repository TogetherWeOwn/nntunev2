-- 0035_mb_mirror_phase2_relations.sql: artist/recording/release relations
-- (credits, song relations), aliases, labels + catalogue numbers, works.
SET search_path = public, pg_catalog;
-- Apply with the manifest runner; it owns the atomic transaction + receipt.
--
-- Phase-2 slice of the CC0-only mirror (ADR 0007): the importer/ETL learn
-- fifteen curated tables behind the existing mirror. Every input is
-- MusicBrainz core dump data (CC0): relationship link tables (l_*), alias
-- tables, label/release_label, work/work_type, label_type and
-- artist_credit_name. The ETL never reads the NC-licensed supplementary
-- tables (tags/genres, ratings, annotations) or the Live Data Feed
-- packets, so this slice imports identically under twice-weekly CC0 dumps.
--
-- Scope notes:
-- - The five relation tables take every link type found in the core dump
--   link tables (no allowlist: link types themselves are core CC0 data).
--   The general recording<->recording table is directed and excludes the
--   music-video link type, which keeps living in mb_recording_music_video
--   (migration 0030) so the two never duplicate a pair.
-- - Release credits project artist_credit_name rows through the release
--   credit only; release-group credits stay deferred.
-- - Alias tables collapse to one row per (entity, name, locale): NULL
--   locales become '' and duplicate spellings collapse with ROW_NUMBER() in
--   the ETL, so the natural-key primary key can never collide within a dump.
-- - release_label rows whose label is NULL are skipped in the ETL (a
--   catalogue number with no label is not a label edge); NULL catalogue
--   numbers become ''.
-- - Cover art stays rights-flagged (no new cover-art caching here) and no
--   read routes change in this card: the tables sit behind the mirror for
--   R2.8/R2.9/R3.5 work to build on.
--
-- Roles: importer owns these tables (same as 0001). No grants here: no
-- migration in this series grants mirror read access, so the MB read role
-- picks the new mb_* tables up the same way it reads the existing ones;
-- verify SELECT on each new table when the migration is applied. Cursor +
-- DLQ stay operator surfaces, never served.
-- Replay-safety: every statement is IF NOT EXISTS / additive; the
-- table_name CHECKs swap in place like 0016/0030 (Postgres auto-named
-- column checks from 0014/0016 are replaced, never edited). The swaps use
-- NOT VALID plus VALIDATE CONSTRAINT in the runner's atomic transaction.
-- Validation retains the swap locks until commit: pause imports and allow
-- for the cursor/DLQ scan. Re-running validation is a no-op success.

-- Works (compositions) and labels come first: the relation and
-- release-label tables below reference them. work_type / label_type are
-- the denormalized type names; label_code is the numeric code printed on
-- releases, NULL when the label has none.
CREATE TABLE IF NOT EXISTS public.mb_work (
    mbid       UUID PRIMARY KEY,
    name       TEXT NOT NULL,
    work_type  TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    line_no    BIGINT
);

CREATE INDEX IF NOT EXISTS mb_work_name_trgm_idx
    ON public.mb_work USING gin (name gin_trgm_ops);

CREATE TABLE IF NOT EXISTS public.mb_label (
    mbid       UUID PRIMARY KEY,
    name       TEXT NOT NULL,
    label_code INTEGER,
    label_type TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    line_no    BIGINT
);

CREATE INDEX IF NOT EXISTS mb_label_name_trgm_idx
    ON public.mb_label USING gin (name gin_trgm_ops);

-- Artist <-> recording / release / work and recording <-> work/song
-- relations. The key is the triple: one entity pair links under several
-- relationship types.
CREATE TABLE IF NOT EXISTS public.mb_artist_recording (
    artist_mbid UUID NOT NULL REFERENCES public.mb_artist (mbid) ON DELETE CASCADE,
    recording_mbid UUID NOT NULL REFERENCES public.mb_recording (mbid) ON DELETE CASCADE,
    link_name   TEXT NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    line_no     BIGINT,
    PRIMARY KEY (artist_mbid, recording_mbid, link_name)
);

CREATE TABLE IF NOT EXISTS public.mb_artist_release (
    artist_mbid UUID NOT NULL REFERENCES public.mb_artist (mbid) ON DELETE CASCADE,
    release_mbid UUID NOT NULL REFERENCES public.mb_release (mbid) ON DELETE CASCADE,
    link_name   TEXT NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    line_no     BIGINT,
    PRIMARY KEY (artist_mbid, release_mbid, link_name)
);

CREATE TABLE IF NOT EXISTS public.mb_artist_work (
    artist_mbid UUID NOT NULL REFERENCES public.mb_artist (mbid) ON DELETE CASCADE,
    work_mbid   UUID NOT NULL REFERENCES public.mb_work (mbid) ON DELETE CASCADE,
    link_name   TEXT NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    line_no     BIGINT,
    PRIMARY KEY (artist_mbid, work_mbid, link_name)
);

CREATE TABLE IF NOT EXISTS public.mb_recording_work (
    recording_mbid UUID NOT NULL REFERENCES public.mb_recording (mbid) ON DELETE CASCADE,
    work_mbid      UUID NOT NULL REFERENCES public.mb_work (mbid) ON DELETE CASCADE,
    link_name      TEXT NOT NULL,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    line_no        BIGINT,
    PRIMARY KEY (recording_mbid, work_mbid, link_name)
);

-- Directed song relations (both listing directions are rows); the
-- music-video link type is excluded here and stays in
-- mb_recording_music_video, so a video pair never lands in both tables.
CREATE TABLE IF NOT EXISTS public.mb_recording_recording (
    recording_a_mbid UUID NOT NULL REFERENCES public.mb_recording (mbid) ON DELETE CASCADE,
    recording_b_mbid UUID NOT NULL REFERENCES public.mb_recording (mbid) ON DELETE CASCADE,
    link_name        TEXT NOT NULL,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    line_no          BIGINT,
    PRIMARY KEY (recording_a_mbid, recording_b_mbid, link_name)
);

CREATE INDEX IF NOT EXISTS mb_recording_recording_b_idx
    ON public.mb_recording_recording (recording_b_mbid);

-- Full release credits: every artist-credit position on the release, in
-- order, with the credited name and join phrase as printed on the release.
CREATE TABLE IF NOT EXISTS public.mb_release_credit (
    release_mbid UUID NOT NULL REFERENCES public.mb_release (mbid) ON DELETE CASCADE,
    position     SMALLINT NOT NULL CHECK (position >= 0),
    artist_mbid  UUID NOT NULL REFERENCES public.mb_artist (mbid) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    join_phrase  TEXT NOT NULL,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    line_no      BIGINT,
    PRIMARY KEY (release_mbid, position)
);

-- Release <-> label edges with catalogue numbers. The key is the triple:
-- one release lists the same label under several catalogue numbers.
CREATE TABLE IF NOT EXISTS public.mb_release_label (
    release_mbid   UUID NOT NULL REFERENCES public.mb_release (mbid) ON DELETE CASCADE,
    label_mbid     UUID NOT NULL REFERENCES public.mb_label (mbid) ON DELETE CASCADE,
    catalog_number TEXT NOT NULL,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    line_no        BIGINT,
    PRIMARY KEY (release_mbid, label_mbid, catalog_number)
);

-- Aliases (search names). One row per (entity, name, locale); the ETL
-- rewrites NULL locales to '' and collapses duplicate spellings, so the
-- natural key holds within a dump. Trigram indexes serve the future
-- alias-search reads (SR-06).
CREATE TABLE IF NOT EXISTS public.mb_artist_alias (
    artist_mbid        UUID NOT NULL REFERENCES public.mb_artist (mbid) ON DELETE CASCADE,
    name               TEXT NOT NULL,
    locale             TEXT NOT NULL,
    sort_name          TEXT NOT NULL,
    primary_for_locale BOOLEAN NOT NULL,
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    line_no            BIGINT,
    PRIMARY KEY (artist_mbid, name, locale)
);

CREATE INDEX IF NOT EXISTS mb_artist_alias_name_trgm_idx
    ON public.mb_artist_alias USING gin (name gin_trgm_ops);

CREATE TABLE IF NOT EXISTS public.mb_release_alias (
    release_mbid       UUID NOT NULL REFERENCES public.mb_release (mbid) ON DELETE CASCADE,
    name               TEXT NOT NULL,
    locale             TEXT NOT NULL,
    sort_name          TEXT NOT NULL,
    primary_for_locale BOOLEAN NOT NULL,
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    line_no            BIGINT,
    PRIMARY KEY (release_mbid, name, locale)
);

CREATE INDEX IF NOT EXISTS mb_release_alias_name_trgm_idx
    ON public.mb_release_alias USING gin (name gin_trgm_ops);

CREATE TABLE IF NOT EXISTS public.mb_recording_alias (
    recording_mbid     UUID NOT NULL REFERENCES public.mb_recording (mbid) ON DELETE CASCADE,
    name               TEXT NOT NULL,
    locale             TEXT NOT NULL,
    sort_name          TEXT NOT NULL,
    primary_for_locale BOOLEAN NOT NULL,
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    line_no            BIGINT,
    PRIMARY KEY (recording_mbid, name, locale)
);

CREATE INDEX IF NOT EXISTS mb_recording_alias_name_trgm_idx
    ON public.mb_recording_alias USING gin (name gin_trgm_ops);

CREATE TABLE IF NOT EXISTS public.mb_release_group_alias (
    release_group_mbid UUID NOT NULL REFERENCES public.mb_release_group (mbid) ON DELETE CASCADE,
    name               TEXT NOT NULL,
    locale             TEXT NOT NULL,
    sort_name          TEXT NOT NULL,
    primary_for_locale BOOLEAN NOT NULL,
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    line_no            BIGINT,
    PRIMARY KEY (release_group_mbid, name, locale)
);

CREATE INDEX IF NOT EXISTS mb_release_group_alias_name_trgm_idx
    ON public.mb_release_group_alias USING gin (name gin_trgm_ops);

CREATE TABLE IF NOT EXISTS public.mb_work_alias (
    work_mbid          UUID NOT NULL REFERENCES public.mb_work (mbid) ON DELETE CASCADE,
    name               TEXT NOT NULL,
    locale             TEXT NOT NULL,
    sort_name          TEXT NOT NULL,
    primary_for_locale BOOLEAN NOT NULL,
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    line_no            BIGINT,
    PRIMARY KEY (work_mbid, name, locale)
);

CREATE INDEX IF NOT EXISTS mb_work_alias_name_trgm_idx
    ON public.mb_work_alias USING gin (name gin_trgm_ops);

CREATE TABLE IF NOT EXISTS public.mb_label_alias (
    label_mbid         UUID NOT NULL REFERENCES public.mb_label (mbid) ON DELETE CASCADE,
    name               TEXT NOT NULL,
    locale             TEXT NOT NULL,
    sort_name          TEXT NOT NULL,
    primary_for_locale BOOLEAN NOT NULL,
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    line_no            BIGINT,
    PRIMARY KEY (label_mbid, name, locale)
);

CREATE INDEX IF NOT EXISTS mb_label_alias_name_trgm_idx
    ON public.mb_label_alias USING gin (name gin_trgm_ops);

-- The import cursor/DLQ table_name checks (migrations 0014, 0016, 0030;
-- Postgres auto-named column checks) learn the fifteen phase-2 tables. The
-- swaps and validation share the runner's transaction and checksum receipt.
-- Their locks remain held through validation; apply with imports paused.
ALTER TABLE public.mb_import_cursor DROP CONSTRAINT IF EXISTS mb_import_cursor_table_name_check;
ALTER TABLE public.mb_import_cursor ADD CONSTRAINT mb_import_cursor_table_name_check
    CHECK (table_name IN ('artist', 'release', 'recording', 'track',
                          'release_group', 'url', 'artist_url', 'recording_url',
                          'recording_music_video', 'recording_isrc',
                          'work', 'label',
                          'artist_recording', 'artist_release', 'artist_work',
                          'recording_work', 'recording_recording',
                          'release_credit',
                          'artist_alias', 'release_alias', 'recording_alias',
                          'release_group_alias', 'work_alias', 'label_alias',
                          'release_label')) NOT VALID;
ALTER TABLE public.mb_import_dlq DROP CONSTRAINT IF EXISTS mb_import_dlq_table_name_check;
ALTER TABLE public.mb_import_dlq ADD CONSTRAINT mb_import_dlq_table_name_check
    CHECK (table_name IN ('artist', 'release', 'recording', 'track',
                          'release_group', 'url', 'artist_url', 'recording_url',
                          'recording_music_video', 'recording_isrc',
                          'work', 'label',
                          'artist_recording', 'artist_release', 'artist_work',
                          'recording_work', 'recording_recording',
                          'release_credit',
                          'artist_alias', 'release_alias', 'recording_alias',
                          'release_group_alias', 'work_alias', 'label_alias',
                          'release_label')) NOT VALID;
-- The manifest runner commits effects and receipt together after validation.
ALTER TABLE public.mb_import_cursor VALIDATE CONSTRAINT mb_import_cursor_table_name_check;
ALTER TABLE public.mb_import_dlq VALIDATE CONSTRAINT mb_import_dlq_table_name_check;
