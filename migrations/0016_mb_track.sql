-- 0016_mb_track: release-to-track membership for the MB mirror.
BEGIN;
--
-- mb_recording keeps one canonical release per recording (the lowest
-- release table id in the ETL), so it cannot answer "which recordings
-- are on release B" for a reissue/compilation B whose tracks first
-- appeared elsewhere, and mb_recording.track_no is the position within
-- one medium (disc), so it cannot order a multi-medium release either.
-- mb_track stores every track appearance the ETL sees, with the medium
-- position, and workers/mb lists a release's recordings from it in
-- (medium_no, track_no) album order. Positions follow MusicBrainz:
-- medium.position and track.position (a pregap track is position 0).
-- The key is the position, not the recording: when a later dump
-- repoints a track at a different recording the upsert replaces it,
-- and the importer keeps only each touched release's newest-dump
-- rows (dropped tracks are pruned; an older dump cannot resurrect
-- them), so no stale row outlives its dump.
-- Roles: importer owns this table (same as 0001). No grants here:
-- no migration in this series grants mirror read access, so the MB
-- read role picks mb_track up the same way it reads the other mb_*
-- tables; verify SELECT on mb_track when the migration is applied.

CREATE TABLE IF NOT EXISTS mb_track (
    release_mbid   UUID NOT NULL REFERENCES mb_release (mbid) ON DELETE CASCADE,
    recording_mbid UUID NOT NULL REFERENCES mb_recording (mbid) ON DELETE CASCADE,
    medium_no      SMALLINT NOT NULL CHECK (medium_no >= 0),
    track_no       SMALLINT NOT NULL CHECK (track_no >= 0),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    line_no        BIGINT,
    PRIMARY KEY (release_mbid, medium_no, track_no)
);

CREATE INDEX IF NOT EXISTS mb_track_recording_idx
    ON mb_track (recording_mbid);

-- The import cursor/DLQ table_name checks (migration 0014, Postgres
-- auto-named column checks) learn the fourth curated table.
ALTER TABLE mb_import_cursor DROP CONSTRAINT IF EXISTS mb_import_cursor_table_name_check;
ALTER TABLE mb_import_cursor ADD CONSTRAINT mb_import_cursor_table_name_check
    CHECK (table_name IN ('artist', 'release', 'recording', 'track'));
ALTER TABLE mb_import_dlq DROP CONSTRAINT IF EXISTS mb_import_dlq_table_name_check;
ALTER TABLE mb_import_dlq ADD CONSTRAINT mb_import_dlq_table_name_check
    CHECK (table_name IN ('artist', 'release', 'recording', 'track'));
COMMIT;
