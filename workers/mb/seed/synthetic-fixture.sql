-- synthetic-fixture.sql: seed the MB mirror with the synthetic-fixture release.
--
-- What: inserts the one release the staging E2E ingests
-- (`e2e/fixtures/synthetic.nzb`, 660 Hz sine track) into the mirror schema
-- (`migrations/0001_mb_mirror.sql` + `0016_mb_track.sql`). The paired
-- seed-verification test
-- (`workers/mb/tests/sql/mirror_seed_synthetic_fixture_verify.sql`)
-- owns the expected rows; this file owns inserting them.
--
-- Fixture rows (the builder contract; do not drift without updating the
-- verification and `e2e/tests/mb-mirror-seed.spec.ts`):
--   mb_artist    11111111-1111-1111-1111-111111111111  'NNTune Fixture Artist'
--   mb_release   22222222-2222-2222-2222-222222222222  'NNTune-Fixture sine-660'
--   mb_recording 33333333-3333-3333-3333-333333333333  same title, track 1, 210 ms
--   mb_track     (release, recording, medium 1, track 1)
-- All MBIDs are unmistakably synthetic (never real MusicBrainz data).
-- The release title is EXACTLY the NZB-derived match query (subject
-- 'NNTune-Fixture sine-660 [01/03] - "sine-660.wav" yEnc' derives to
-- 'NNTune-Fixture sine-660' via derive_upload_query in
-- workers/ingest/src/matching.rs); any other title never matches.
--
-- Re-runnable: every statement is an idempotent upsert keyed on the row's
-- natural key (artist/release/recording MBID; track position). Re-running
-- inserts nothing new and repairs a drifted title/name/duration back to
-- the fixture. Safe to apply after every mirror import that might have
-- pruned or overwritten these rows.
--
-- No credentials in this file: the caller supplies the connection via the
-- DATABASE_URL environment variable only (see workers/mb/seed/README.md).
-- Never point this at production. The staging apply is out of scope here:
-- an authorized host executor applies it under the existing mirror access
-- grant and re-runs the verification read-only as evidence.

-- 1. Fixture artist.
INSERT INTO mb_artist (mbid, name, sort_name)
VALUES ('11111111-1111-1111-1111-111111111111'::uuid,
        'NNTune Fixture Artist',
        'NNTune Fixture Artist')
ON CONFLICT (mbid) DO UPDATE
    SET name = EXCLUDED.name,
        sort_name = EXCLUDED.sort_name,
        updated_at = now();

-- 2. Fixture release (linked artist).
INSERT INTO mb_release (mbid, artist_mbid, title)
VALUES ('22222222-2222-2222-2222-222222222222'::uuid,
        '11111111-1111-1111-1111-111111111111'::uuid,
        'NNTune-Fixture sine-660')
ON CONFLICT (mbid) DO UPDATE
    SET artist_mbid = EXCLUDED.artist_mbid,
        title = EXCLUDED.title,
        updated_at = now();

-- 3. Fixture recording (canonical release + position + length).
INSERT INTO mb_recording (mbid, release_mbid, title, track_no, duration_ms)
VALUES ('33333333-3333-3333-3333-333333333333'::uuid,
        '22222222-2222-2222-2222-222222222222'::uuid,
        'NNTune-Fixture sine-660',
        1,
        210)
ON CONFLICT (mbid) DO UPDATE
    SET release_mbid = EXCLUDED.release_mbid,
        title = EXCLUDED.title,
        track_no = EXCLUDED.track_no,
        duration_ms = EXCLUDED.duration_ms,
        updated_at = now();

-- 4. Track membership: the one appearance (medium 1, track 1). The key is
-- the position, so a later dump repointing this slot is replaced, and an
-- older dump can never resurrect a stale row here.
INSERT INTO mb_track (release_mbid, recording_mbid, medium_no, track_no)
VALUES ('22222222-2222-2222-2222-222222222222'::uuid,
        '33333333-3333-3333-3333-333333333333'::uuid,
        1,
        1)
ON CONFLICT (release_mbid, medium_no, track_no) DO UPDATE
    SET recording_mbid = EXCLUDED.recording_mbid,
        updated_at = now();
