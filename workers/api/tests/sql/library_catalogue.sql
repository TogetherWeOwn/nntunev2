-- Regression test for the catalogue slice-1 reads
-- (workers/api/src/catalogue.rs against migration 0026 plus the mirror
-- 0001/0016). Run after ALL migrations are applied (see
-- ../run_migration_tests.sh). Everything happens inside one transaction
-- that is rolled back, so it leaves no rows; any broken expectation RAISEs
-- and (with ON_ERROR_STOP) fails the run.
--
-- This test executes the read statements in their exact shapes (literals
-- stand in for the $n parameters): the per-user library page (matched
-- content first, album order, unmatched rows kept, poisoned stored MBIDs
-- degrading to NULL enrichment instead of aborting the page), the LIMIT
-- bound, per-user isolation, the three mirror collections, and the release
-- detail read (existence probe plus album-ordered tracks).

BEGIN;

DO $$
DECLARE
    actual TEXT[];
    row_count_var INTEGER;
BEGIN
    INSERT INTO users (id, email, created_at)
    VALUES ('user-lib-1', 'lib-1@example.com', 1700000000),
           ('user-lib-2', 'lib-2@example.com', 1700000000);

    INSERT INTO mb_artist (mbid, name, sort_name, type, country)
    VALUES ('ffffffff-1111-2222-3333-444444444444'::uuid, 'Fixture Artist',
            'Artist, Fixture', 'Group', 'GB'),
           ('eeeeeeee-1111-2222-3333-444444444444'::uuid, 'Second Artist',
            'Artist, Second', NULL, NULL);

    INSERT INTO mb_release (mbid, artist_mbid, title, release_type, year, caa_url)
    VALUES ('aaaaaaaa-1111-2222-3333-444444444444'::uuid,
            'ffffffff-1111-2222-3333-444444444444'::uuid,
            'Fixture Release', 'Album', 1983,
            'https://coverartarchive.example/release/aaaaaaaa');

    INSERT INTO mb_recording (mbid, release_mbid, title, track_no, duration_ms)
    VALUES ('11111111-1111-2222-3333-444444444444'::uuid,
            'aaaaaaaa-1111-2222-3333-444444444444'::uuid, 'Track One', 1, 200000),
           ('22222222-1111-2222-3333-444444444444'::uuid,
            'aaaaaaaa-1111-2222-3333-444444444444'::uuid, 'Track Two', 2, 210000),
           ('33333333-1111-2222-3333-444444444444'::uuid,
            'aaaaaaaa-1111-2222-3333-444444444444'::uuid, 'Track Three', 3, 220000);

    INSERT INTO mb_track (release_mbid, recording_mbid, medium_no, track_no)
    VALUES ('aaaaaaaa-1111-2222-3333-444444444444'::uuid,
            '11111111-1111-2222-3333-444444444444'::uuid, 1, 1),
           ('aaaaaaaa-1111-2222-3333-444444444444'::uuid,
            '22222222-1111-2222-3333-444444444444'::uuid, 1, 2),
           ('aaaaaaaa-1111-2222-3333-444444444444'::uuid,
            '33333333-1111-2222-3333-444444444444'::uuid, 1, 3);

    -- user-lib-1: one fully matched job (3 recordings), one matched job
    -- whose release the mirror does not carry, one uppercase-release job
    -- (uuid comparison is case-insensitive), one poisoned job (malformed
    -- stored MBIDs must degrade, never abort), one unmatched job.
    INSERT INTO library (idempotency_key, job_id, user_id, release_mbid, recording_mbids)
    VALUES ('lib:key:A', 'lib-job-A', 'user-lib-1',
            'aaaaaaaa-1111-2222-3333-444444444444',
            '{11111111-1111-2222-3333-444444444444,22222222-1111-2222-3333-444444444444,33333333-1111-2222-3333-444444444444}'),
           ('lib:key:B', 'lib-job-B', 'user-lib-1',
            'bbbbbbbb-1111-2222-3333-444444444444',
            '{99999999-1111-2222-3333-444444444444}'),
           ('lib:key:E', 'lib-job-E', 'user-lib-1',
            'AAAAAAAA-1111-2222-3333-444444444444',
            '{11111111-1111-2222-3333-444444444444}'),
           ('lib:key:D', 'lib-job-D', 'user-lib-1',
            'not-a-uuid', '{also-not-a-uuid}'),
           ('lib:key:C', 'lib-job-C', 'user-lib-1', NULL, '{}');

    -- user-lib-2: 55 single-track jobs for the LIMIT bound pin.
    INSERT INTO library (idempotency_key, job_id, user_id, release_mbid, recording_mbids)
    SELECT 'lib:key:bound:' || g, 'lib-bound-' || lpad(g::text, 3, '0'), 'user-lib-2',
           'aaaaaaaa-1111-2222-3333-444444444444',
           '{11111111-1111-2222-3333-444444444444}'::text[]
    FROM generate_series(1, 55) AS g;

    -- Library page for user-lib-1 (literals stand in for $1/$2).
    CREATE TEMPORARY TABLE lib_page AS
    SELECT l.job_id, l.release_mbid, l.recording_mbids, u.recording_mbid, u.recording_ord,
           rec.title AS recording_title, rec.duration_ms, t.medium_no, t.track_no,
           r.title AS release_title, r.artist_mbid::text AS artist_mbid, a.name AS artist_name
    FROM library l
    LEFT JOIN LATERAL (SELECT value AS recording_mbid, ord AS recording_ord,
           CASE WHEN value ~ '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
           THEN value::uuid END AS recording_uuid
           FROM unnest(l.recording_mbids) WITH ORDINALITY AS u2(value, ord)) AS u ON true
    LEFT JOIN mb_recording rec ON rec.mbid = u.recording_uuid
    LEFT JOIN mb_track t ON t.release_mbid = CASE WHEN l.release_mbid ~
           '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
           THEN l.release_mbid::uuid END AND t.recording_mbid = rec.mbid
    LEFT JOIN mb_release r ON r.mbid = CASE WHEN l.release_mbid ~
           '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
           THEN l.release_mbid::uuid END
    LEFT JOIN mb_artist a ON a.mbid = r.artist_mbid
    WHERE l.user_id = 'user-lib-1' AND coalesce(cardinality(l.recording_mbids), 0) > 0
    UNION ALL
    SELECT l.job_id, l.release_mbid, l.recording_mbids, NULL::text AS recording_mbid,
           0::bigint AS recording_ord, NULL::text AS recording_title, NULL::integer AS duration_ms,
           NULL::smallint AS medium_no, NULL::smallint AS track_no, r.title AS release_title,
           r.artist_mbid::text AS artist_mbid, a.name AS artist_name
    FROM library l
    LEFT JOIN mb_release r ON r.mbid = CASE WHEN l.release_mbid ~
           '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
           THEN l.release_mbid::uuid END
    LEFT JOIN mb_artist a ON a.mbid = r.artist_mbid
    WHERE l.user_id = 'user-lib-1' AND coalesce(cardinality(l.recording_mbids), 0) = 0
    ORDER BY release_mbid NULLS LAST, job_id DESC, recording_ord ASC LIMIT 50;

    -- Matched-first (job DESC within), album order within a job, the
    -- unmatched upload last. ORDER BY lives inside array_agg: the temp
    -- table's heap order is not a contract.
    SELECT array_agg(job_id || ':' || recording_ord::text
                     ORDER BY (release_mbid IS NULL), job_id DESC, recording_ord ASC)
    INTO actual FROM lib_page;
    IF actual <> ARRAY['lib-job-E:1', 'lib-job-D:1', 'lib-job-B:1',
                       'lib-job-A:1', 'lib-job-A:2', 'lib-job-A:3',
                       'lib-job-C:0'] THEN
        RAISE EXCEPTION 'library page order wrong: %', actual;
    END IF;

    -- Uppercase stored MBIDs still enrich (uuid comparison, not text).
    SELECT array_agg(recording_title) INTO actual
    FROM (SELECT recording_title FROM lib_page WHERE job_id = 'lib-job-E') s;
    IF actual <> ARRAY['Track One'] THEN
        RAISE EXCEPTION 'uppercase release MBID did not enrich: %', actual;
    END IF;

    -- Unknown-but-well-formed MBIDs degrade every enrichment to NULL.
    SELECT count(*) INTO row_count_var FROM lib_page
    WHERE job_id = 'lib-job-B'
      AND recording_title IS NULL AND release_title IS NULL
      AND artist_name IS NULL AND medium_no IS NULL;
    IF row_count_var <> 1 THEN
        RAISE EXCEPTION 'unknown MBIDs did not degrade to NULL enrichment';
    END IF;

    -- Poisoned stored MBIDs degrade too (and reaching this line proves the
    -- CASE guards evaluated before any text::uuid cast: no error raised).
    SELECT count(*) INTO row_count_var FROM lib_page
    WHERE job_id = 'lib-job-D'
      AND recording_mbid = 'also-not-a-uuid'
      AND recording_title IS NULL AND release_title IS NULL;
    IF row_count_var <> 1 THEN
        RAISE EXCEPTION 'poisoned MBIDs did not degrade to NULL enrichment';
    END IF;

    -- The unmatched upload stays visible with NULL recording/release.
    SELECT count(*) INTO row_count_var FROM lib_page
    WHERE job_id = 'lib-job-C'
      AND recording_mbid IS NULL AND release_mbid IS NULL
      AND cardinality(recording_mbids) = 0;
    IF row_count_var <> 1 THEN
        RAISE EXCEPTION 'unmatched library row missing or misshaped';
    END IF;

    -- Occurrence positions ride the matched rows.
    SELECT array_agg(medium_no::text || '.' || track_no::text ORDER BY recording_ord) INTO actual
    FROM (SELECT medium_no, track_no, recording_ord FROM lib_page WHERE job_id = 'lib-job-A') s;
    IF actual <> ARRAY['1.1', '1.2', '1.3'] THEN
        RAISE EXCEPTION 'occurrence positions wrong: %', actual;
    END IF;

    -- Per-user isolation: no user-lib-2 row may appear on the lib-1 page.
    SELECT count(*) INTO row_count_var FROM lib_page WHERE job_id LIKE 'lib-bound-%';
    IF row_count_var <> 0 THEN
        RAISE EXCEPTION 'library page leaked another user''s rows';
    END IF;

    -- LIMIT bound: 55 jobs in, exactly 50 rows out.
    SELECT count(*) INTO row_count_var FROM (
        SELECT l.job_id FROM library l
        WHERE l.user_id = 'user-lib-2'
        ORDER BY l.release_mbid NULLS LAST, l.job_id DESC LIMIT 50
    ) s;
    IF row_count_var <> 50 THEN
        RAISE EXCEPTION 'library bound did not cap at 50: %', row_count_var;
    END IF;

    -- Artist collection: MBID order, CHAR(2) country decoded.
    SELECT array_agg(mbid::text ORDER BY mbid) INTO actual FROM (
        SELECT mbid::text AS mbid, name, sort_name, type, country::text AS country
        FROM mb_artist ORDER BY mbid ASC LIMIT 50
    ) s;
    IF actual <> ARRAY['eeeeeeee-1111-2222-3333-444444444444',
                       'ffffffff-1111-2222-3333-444444444444'] THEN
        RAISE EXCEPTION 'artist collection order wrong: %', actual;
    END IF;

    -- Release collection carries the artist join.
    SELECT count(*) INTO row_count_var FROM (
        SELECT r.mbid::text AS mbid, r.title, r.release_type, r.year,
               r.artist_mbid::text AS artist_mbid, a.name AS artist_name
        FROM mb_release r LEFT JOIN mb_artist a ON a.mbid = r.artist_mbid
        ORDER BY r.mbid ASC LIMIT 50
    ) s WHERE mbid = 'aaaaaaaa-1111-2222-3333-444444444444'
      AND artist_name = 'Fixture Artist' AND year = 1983;
    IF row_count_var <> 1 THEN
        RAISE EXCEPTION 'release collection missed the artist join';
    END IF;

    -- Track collection: release, then true album position.
    -- (track_no is SMALLINT: cast to text so the TEXT[] accumulator matches.)
    SELECT array_agg(track_no::text ORDER BY release_mbid, medium_no, track_no) INTO actual FROM (
        SELECT t.release_mbid::text AS release_mbid, t.recording_mbid::text AS recording_mbid,
               rec.title, rec.duration_ms, t.medium_no, t.track_no, r.title AS release_title,
               r.artist_mbid::text AS artist_mbid, a.name AS artist_name
        FROM mb_track t JOIN mb_recording rec ON rec.mbid = t.recording_mbid
        JOIN mb_release r ON r.mbid = t.release_mbid
        LEFT JOIN mb_artist a ON a.mbid = r.artist_mbid
        ORDER BY t.release_mbid ASC, t.medium_no ASC, t.track_no ASC LIMIT 50
    ) s;
    IF actual <> ARRAY['1', '2', '3'] THEN
        RAISE EXCEPTION 'track collection order wrong: %', actual;
    END IF;

    -- Release detail: known reads one row, unknown reads none (the 404
    -- probe), tracks arrive in album order.
    SELECT count(*) INTO row_count_var FROM (
        SELECT r.mbid::text AS mbid, r.title, r.release_type, r.year,
               r.artist_mbid::text AS artist_mbid, a.name AS artist_name
        FROM mb_release r LEFT JOIN mb_artist a ON a.mbid = r.artist_mbid
        WHERE r.mbid = 'aaaaaaaa-1111-2222-3333-444444444444'::text::uuid
    ) s;
    IF row_count_var <> 1 THEN
        RAISE EXCEPTION 'release detail missed a known release';
    END IF;
    SELECT count(*) INTO row_count_var FROM (
        SELECT r.mbid::text AS mbid FROM mb_release r
        WHERE r.mbid = 'cccccccc-1111-2222-3333-444444444444'::text::uuid
    ) s;
    IF row_count_var <> 0 THEN
        RAISE EXCEPTION 'release detail found an unknown release';
    END IF;
    SELECT array_agg(recording_mbid ORDER BY medium_no, track_no, recording_mbid) INTO actual FROM (
        SELECT t.recording_mbid::text AS recording_mbid, rec.title, rec.duration_ms,
               t.medium_no, t.track_no
        FROM mb_track t JOIN mb_recording rec ON rec.mbid = t.recording_mbid
        WHERE t.release_mbid = 'aaaaaaaa-1111-2222-3333-444444444444'::text::uuid
        ORDER BY t.medium_no ASC, t.track_no ASC, rec.mbid ASC LIMIT 1000
    ) s;
    IF actual <> ARRAY['11111111-1111-2222-3333-444444444444',
                       '22222222-1111-2222-3333-444444444444',
                       '33333333-1111-2222-3333-444444444444'] THEN
        RAISE EXCEPTION 'release tracks order wrong: %', actual;
    END IF;
END $$;

ROLLBACK;
