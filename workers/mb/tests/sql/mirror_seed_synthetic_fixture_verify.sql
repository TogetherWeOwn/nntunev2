-- Seed verification for TOG-28772 (acceptance-test card).
--
-- What: proves the MB mirror holds the synthetic-fixture release the
-- staging E2E ingests (`e2e/fixtures/synthetic.nzb`, 660 Hz sine track).
-- Fails by name when the mirror lacks the fixture rows (empty DB) or
-- holds wrong ones (drifted MBID/title/membership); passes only on the
-- exact seeded fixture. The paired build card (TOG-28778) owns the seed
-- itself and never edits this file.
--
-- Fixture rows (the builder contract; the seed must insert exactly these):
--   mb_artist   11111111-1111-1111-1111-111111111111  'NNTune Fixture Artist'
--   mb_release  22222222-2222-2222-2222-222222222222  'NNTune-Fixture sine-660'
--     (title is EXACTLY the NZB-derived match query: subject
--     'NNTune-Fixture sine-660 [01/03] - "sine-660.wav" yEnc' derives to
--     'NNTune-Fixture sine-660' via derive_upload_query in
--     workers/ingest/src/matching.rs; any other title never matches)
--   mb_recording 33333333-3333-3333-3333-333333333333  same title,
--     track_no 1, duration_ms 210 (the 210 ms synthetic WAV)
--   mb_track    (release, recording, medium_no 1, track_no 1)
-- All MBIDs are valid 8-4-4-4-12 hex (see the isMbid pins in
-- e2e/tests/helpers.spec.ts) and unmistakably synthetic: they can never
-- collide with real MusicBrainz data.
--
-- Local Postgres setup (scratch DB only; never staging/prod):
--   createdb mb_seedcheck
--   export DATABASE_URL=postgres://$USER@localhost/mb_seedcheck
--   for m in 0001_mb_mirror 0014_mb_import_resume_dlq 0016_mb_track \
--            0017_mb_replication_packet_ledger; do
--     psql "$DATABASE_URL" -X -q -v ON_ERROR_STOP=1 -f migrations/${m}.sql
--   done
--   # Empty DB: this script MUST fail (missing rows fail by name).
--   psql "$DATABASE_URL" -X -q -v ON_ERROR_STOP=1 \
--     -f workers/mb/tests/sql/mirror_seed_synthetic_fixture_verify.sql
--   # Apply the builder seed (TOG-28778), re-run: it MUST pass.
--   # Re-run the seed, re-verify: still green (idempotent upsert).
--
-- Read-only: everything runs inside one transaction that is rolled back,
-- so verification leaves no rows. Any broken expectation RAISEs and (with
-- ON_ERROR_STOP) fails the run.
--
-- The release-recordings check below runs the same membership query the
-- route serves (`RELEASE_RECORDINGS_SQL` in workers/mb/src/lib.rs:
-- mb_track join, album order, LIMIT 1000), so a green run means
-- GET /v1/mb/release/<fixture>/recordings answers the fixture recording.
-- The similarity check pins the matcher's plain-title gate
-- (MIN_MATCH_SCORE 0.6 in workers/ingest/src/matching.rs): the seeded
-- title must score at or above it for the derived query, or the ingest
-- match never fires even with the rows present.

BEGIN;

DO $$
DECLARE
    v_artist_mbid    CONSTANT text := '11111111-1111-1111-1111-111111111111';
    v_release_mbid   CONSTANT text := '22222222-2222-2222-2222-222222222222';
    v_recording_mbid CONSTANT text := '33333333-3333-3333-3333-333333333333';
    v_artist_name    CONSTANT text := 'NNTune Fixture Artist';
    v_release_title  CONSTANT text := 'NNTune-Fixture sine-660';
    v_hits           integer;
    v_score          double precision;
BEGIN
    -- 1. Fixture artist: exactly one row, exact name.
    SELECT count(*) INTO v_hits FROM mb_artist
        WHERE mbid = v_artist_mbid::text::uuid;
    IF v_hits = 0 THEN
        RAISE EXCEPTION 'mirror seed missing mb_artist row mbid=% (empty or unseeded mirror)', v_artist_mbid;
    ELSIF v_hits > 1 THEN
        RAISE EXCEPTION 'mirror seed duplicated mb_artist row mbid=% (count=%)', v_artist_mbid, v_hits;
    END IF;
    IF (SELECT name FROM mb_artist WHERE mbid = v_artist_mbid::text::uuid) IS DISTINCT FROM v_artist_name THEN
        RAISE EXCEPTION 'mirror seed mb_artist row mbid=% has wrong name (want %)',
            v_artist_mbid, v_artist_name;
    END IF;

    -- 2. Fixture release: exactly one row, exact title, linked artist.
    SELECT count(*) INTO v_hits FROM mb_release
        WHERE mbid = v_release_mbid::text::uuid;
    IF v_hits = 0 THEN
        RAISE EXCEPTION 'mirror seed missing mb_release row mbid=% title=% (empty or unseeded mirror)',
            v_release_mbid, v_release_title;
    ELSIF v_hits > 1 THEN
        RAISE EXCEPTION 'mirror seed duplicated mb_release row mbid=% (count=%)', v_release_mbid, v_hits;
    END IF;
    IF (SELECT title FROM mb_release WHERE mbid = v_release_mbid::text::uuid) IS DISTINCT FROM v_release_title THEN
        RAISE EXCEPTION 'mirror seed mb_release row mbid=% has wrong title (want %: the NZB-derived match query)',
            v_release_mbid, v_release_title;
    END IF;
    IF (SELECT artist_mbid::text FROM mb_release WHERE mbid = v_release_mbid::text::uuid)
       IS DISTINCT FROM v_artist_mbid THEN
        RAISE EXCEPTION 'mirror seed mb_release row mbid=% points at wrong artist (want artist_mbid=%)',
            v_release_mbid, v_artist_mbid;
    END IF;

    -- 3. Fixture recording: exactly one row, exact title/position/length.
    SELECT count(*) INTO v_hits FROM mb_recording
        WHERE mbid = v_recording_mbid::text::uuid;
    IF v_hits = 0 THEN
        RAISE EXCEPTION 'mirror seed missing mb_recording row mbid=% (empty or unseeded mirror)', v_recording_mbid;
    ELSIF v_hits > 1 THEN
        RAISE EXCEPTION 'mirror seed duplicated mb_recording row mbid=% (count=%)', v_recording_mbid, v_hits;
    END IF;
    IF (SELECT title FROM mb_recording WHERE mbid = v_recording_mbid::text::uuid) IS DISTINCT FROM v_release_title THEN
        RAISE EXCEPTION 'mirror seed mb_recording row mbid=% has wrong title (want %)',
            v_recording_mbid, v_release_title;
    END IF;
    IF (SELECT duration_ms FROM mb_recording WHERE mbid = v_recording_mbid::text::uuid) IS DISTINCT FROM 210 THEN
        RAISE EXCEPTION 'mirror seed mb_recording row mbid=% has wrong duration_ms (want 210 for the 210 ms synthetic WAV)',
            v_recording_mbid;
    END IF;

    -- 4. Track membership: exactly the one appearance (medium 1, track 1).
    SELECT count(*) INTO v_hits FROM mb_track
        WHERE release_mbid = v_release_mbid::text::uuid;
    IF v_hits = 0 THEN
        RAISE EXCEPTION 'mirror seed missing mb_track membership for release_mbid=% (release with no tracks reads as 200-empty on the route)',
            v_release_mbid;
    ELSIF v_hits > 1 THEN
        RAISE EXCEPTION 'mirror seed holds % mb_track rows for release_mbid=% (want exactly the one fixture appearance)',
            v_hits, v_release_mbid;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM mb_track
                   WHERE release_mbid = v_release_mbid::text::uuid
                     AND recording_mbid = v_recording_mbid::text::uuid
                     AND medium_no = 1 AND track_no = 1) THEN
        RAISE EXCEPTION 'mirror seed mb_track row for release_mbid=% is wrong (want recording_mbid=% medium_no=1 track_no=1)',
            v_release_mbid, v_recording_mbid;
    END IF;

    -- 5. Route shape: the release-recordings membership query (same JOIN /
    -- album order / LIMIT the route serves) returns the fixture recording,
    -- non-empty, first in album order.
    SELECT count(*) INTO v_hits
        FROM mb_track t JOIN mb_recording r ON r.mbid = t.recording_mbid
        WHERE t.release_mbid = v_release_mbid::text::uuid;
    IF v_hits = 0 THEN
        RAISE EXCEPTION 'release-recordings listing for release_mbid=% is empty (route would answer 200 with no recordings)',
            v_release_mbid;
    END IF;
    IF (SELECT r.mbid::text
        FROM mb_track t JOIN mb_recording r ON r.mbid = t.recording_mbid
        WHERE t.release_mbid = v_release_mbid::text::uuid
        ORDER BY t.medium_no ASC, t.track_no ASC, r.mbid ASC LIMIT 1000) IS DISTINCT FROM v_recording_mbid THEN
        RAISE EXCEPTION 'release-recordings listing for release_mbid=% does not lead with recording_mbid=%',
            v_release_mbid, v_recording_mbid;
    END IF;

    -- 6. Match gate: the seeded title must clear the matcher's plain-title
    -- score floor for the NZB-derived query, or the rows sit unmatched.
    SELECT similarity(title, v_release_title) INTO v_score FROM mb_release
        WHERE mbid = v_release_mbid::text::uuid;
    IF v_score IS NULL OR v_score < 0.6 THEN
        RAISE EXCEPTION 'seeded release title scores % for the derived query (need >= 0.6 MIN_MATCH_SCORE)', v_score;
    END IF;
END
$$;

ROLLBACK;
