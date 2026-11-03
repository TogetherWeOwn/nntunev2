-- 0027_mb_mirror_w6b: release groups, URL relationships, music videos, ISRCs.
BEGIN;
--
-- W6b slice of the CC0-only mirror (ADR 0007): release groups (with the
-- release -> group link), curated recording/artist URL relationships
-- (entity-URL pairs resolved through the MusicBrainz relationship tables,
-- filtered to an allowlist of link-type GIDs pinned in the ETL), canonical
-- recording <-> recording music-video pairs, and recording ISRCs. All six
-- inputs are MusicBrainz core data (CC0); the ETL never touches the
-- NC-licensed Live Data Feed packets or the supplementary tables
-- (tags/genres, ratings, annotations), so this slice imports identically
-- under twice-weekly CC0 dumps.
--
-- Roles: importer owns these tables (same as 0001). No grants here: no
-- migration in this series grants mirror read access, so the MB read role
-- picks the new mb_* tables up the same way it reads the existing ones;
-- verify SELECT on each new table when the migration is applied. Cursor +
-- DLQ stay operator surfaces, never served.
-- Replay-safety: every statement is IF NOT EXISTS / additive; the
-- table_name CHECKs swap in place like 0016 (Postgres auto-named column
-- checks from 0014/0016 are replaced, never edited).

-- Release groups ------------------------------------------------------
CREATE TABLE IF NOT EXISTS mb_release_group (
    mbid       UUID PRIMARY KEY,
    name       TEXT NOT NULL,
    artist_mbid UUID REFERENCES mb_artist (mbid) ON DELETE SET NULL,
    "type"     TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    line_no    BIGINT
);

-- The release -> group link. NULL until the release is (re-)imported
-- from a dump carrying the group column: pre-0027 rows keep serving
-- without a group rather than failing the migration.
ALTER TABLE mb_release ADD COLUMN IF NOT EXISTS release_group_mbid
    UUID REFERENCES mb_release_group (mbid) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS mb_release_release_group_idx
    ON mb_release (release_group_mbid);

-- Canonical URLs referenced by the curated relationships ----------------
-- Only URLs targeted by an allowlisted relationship link land here, not
-- the whole upstream url table (see containers/mb-import/etl.py).
CREATE TABLE IF NOT EXISTS mb_url (
    mbid       UUID PRIMARY KEY,
    url        TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    line_no    BIGINT
);

-- Artist <-> URL relationships (allowlisted link types only).
-- The key is the triple: one artist links the same URL under two
-- different relationship types.
CREATE TABLE IF NOT EXISTS mb_artist_url (
    artist_mbid UUID NOT NULL REFERENCES mb_artist (mbid) ON DELETE CASCADE,
    url_mbid    UUID NOT NULL REFERENCES mb_url (mbid) ON DELETE CASCADE,
    link_name   TEXT NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    line_no     BIGINT,
    PRIMARY KEY (artist_mbid, url_mbid, link_name)
);

-- Recording <-> URL relationships (allowlisted link types only).
-- is_video marks links carrying the MusicBrainz "video" link attribute:
-- the music-video -> stream join follows only is_video links.
CREATE TABLE IF NOT EXISTS mb_recording_url (
    recording_mbid UUID NOT NULL REFERENCES mb_recording (mbid) ON DELETE CASCADE,
    url_mbid       UUID NOT NULL REFERENCES mb_url (mbid) ON DELETE CASCADE,
    link_name      TEXT NOT NULL,
    is_video       BOOLEAN NOT NULL,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    line_no        BIGINT,
    PRIMARY KEY (recording_mbid, url_mbid, link_name)
);

-- Recording <-> recording music-video pairs, canonically ordered so one
-- undirected link is one row whichever side the dump lists first. The
-- read join treats both sides symmetrically, so a misdirected upstream
-- link still resolves instead of silently dropping the video.
CREATE TABLE IF NOT EXISTS mb_recording_music_video (
    recording_a_mbid UUID NOT NULL REFERENCES mb_recording (mbid) ON DELETE CASCADE,
    recording_b_mbid UUID NOT NULL REFERENCES mb_recording (mbid) ON DELETE CASCADE,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    line_no          BIGINT,
    PRIMARY KEY (recording_a_mbid, recording_b_mbid),
    CHECK (recording_a_mbid < recording_b_mbid)
);

CREATE INDEX IF NOT EXISTS mb_recording_music_video_b_idx
    ON mb_recording_music_video (recording_b_mbid);

-- Recording ISRCs. The code is the 12-character uppercased form
-- (2-letter country + 3-alphanumeric registrant + 7-digit year/designation).
CREATE TABLE IF NOT EXISTS mb_recording_isrc (
    recording_mbid UUID NOT NULL REFERENCES mb_recording (mbid) ON DELETE CASCADE,
    isrc           TEXT NOT NULL CHECK (isrc ~ '^[A-Z]{2}[A-Z0-9]{3}[0-9]{7}$'),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    line_no        BIGINT,
    PRIMARY KEY (recording_mbid, isrc)
);

-- The import cursor/DLQ table_name checks (migrations 0014, 0016;
-- Postgres auto-named column checks) learn the six curated tables.
ALTER TABLE mb_import_cursor DROP CONSTRAINT IF EXISTS mb_import_cursor_table_name_check;
ALTER TABLE mb_import_cursor ADD CONSTRAINT mb_import_cursor_table_name_check
    CHECK (table_name IN ('artist', 'release', 'recording', 'track',
                          'release_group', 'url', 'artist_url', 'recording_url',
                          'recording_music_video', 'recording_isrc'));
ALTER TABLE mb_import_dlq DROP CONSTRAINT IF EXISTS mb_import_dlq_table_name_check;
ALTER TABLE mb_import_dlq ADD CONSTRAINT mb_import_dlq_table_name_check
    CHECK (table_name IN ('artist', 'release', 'recording', 'track',
                          'release_group', 'url', 'artist_url', 'recording_url',
                          'recording_music_video', 'recording_isrc'));
COMMIT;
