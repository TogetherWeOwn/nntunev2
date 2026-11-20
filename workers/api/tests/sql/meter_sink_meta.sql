-- Regression test for the meter-sink metadata statements
-- (workers/api/src/meter_sink.rs). Run
-- after ALL migrations are applied (see ../run_migration_tests.sh).
-- Everything happens inside one transaction that is rolled back, so it
-- leaves no rows; any broken expectation RAISEs and (with
-- ON_ERROR_STOP) fails the run.
--
-- The functions below mirror the Rust statements' parameters: local track
-- id and user id for the app query, recording MBID for the mirror. They pin:
-- the duration rungs read separately from tracks / mb_recording; the
-- bitrate and file-size rungs pick the CALLER's most recently verified
-- media row (another user's newer row must not leak in); a track with
-- no durations or media still yields its one row with NULLs; an
-- unknown track yields none (the fallback rung meters it, never an
-- error).

BEGIN;

CREATE FUNCTION pg_temp.meter_meta(TEXT, TEXT)
RETURNS TABLE (track_duration_ms INTEGER, track_mbid TEXT,
               bitrate_kbps INTEGER, file_bytes BIGINT, occurrence_ambiguous BOOLEAN)
LANGUAGE SQL AS $function$
    SELECT t.duration_ms AS track_duration_ms, t.mbid::text AS track_mbid,
     (SELECT m.bitrate FROM track_media m WHERE m.track_id = t.id AND m.user_id = $2
     ORDER BY m.verified_at DESC, m.content_hash ASC LIMIT 1) AS bitrate_kbps,
     (SELECT c.byte_size FROM track_media m JOIN cache_objects c
     ON c.content_hash = m.content_hash WHERE m.track_id = t.id AND m.user_id = $2
     ORDER BY m.verified_at DESC, m.content_hash ASC LIMIT 1) AS file_bytes,
     (t.id <> $1::uuid AND (SELECT COUNT(*) FROM tracks t2 WHERE t2.mbid = $1::uuid) > 1)
     AS occurrence_ambiguous
     FROM tracks t WHERE t.id = $1::uuid OR t.mbid = $1::uuid
     ORDER BY (t.id = $1::uuid) DESC, t.id LIMIT 1;
$function$;

CREATE FUNCTION pg_temp.meter_mirror(TEXT)
RETURNS TABLE (mb_duration_ms INTEGER)
LANGUAGE SQL AS $function$
    SELECT duration_ms AS mb_duration_ms FROM mb_recording WHERE mbid = $1::uuid;
$function$;

DO $$
DECLARE
    got_track_ms INTEGER;
    got_mb_ms INTEGER;
    got_mbid TEXT;
    got_bitrate INTEGER;
    got_bytes BIGINT;
    row_count INTEGER;
    ambiguous BOOLEAN;
BEGIN
    INSERT INTO users (id, email, created_at)
    VALUES ('user-ms-1', 'ms-1@example.com', 1700000000),
           ('user-ms-2', 'ms-2@example.com', 1700000000),
           ('user-ms-3', 'ms-3@example.com', 1700000000);

    INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
    VALUES ('job_ms1', 'user-ms-1', 'ms-key-1', 1700000000, 'done'),
           ('job_ms2', 'user-ms-2', 'ms-key-2', 1700000000, 'done');

    INSERT INTO mb_recording (mbid, title, duration_ms)
    VALUES ('33333333-3333-3333-3333-333333333333', 'Meter Track', 241000);

    INSERT INTO tracks (id, mbid, release_id, title, medium_no, track_no, duration_ms)
    VALUES ('22222222-2222-2222-2222-222222222222',
            '33333333-3333-3333-3333-333333333333',
            '44444444-4444-4444-4444-444444444444',
            'Meter Track', 1, 1, 240000),
           ('55555555-5555-5555-5555-555555555555',
            NULL,
            '44444444-4444-4444-4444-444444444444',
            'Bare Track', 1, 2, NULL);

    INSERT INTO cache_objects (content_hash, byte_size, r2_bucket, r2_key, last_verified_at)
    VALUES (repeat('a', 64), 1000000, 'bucket', 'audio/a', 1700000000),
           (repeat('b', 64), 2000000, 'bucket', 'audio/b', 1700000000),
           (repeat('c', 64), 3000000, 'bucket', 'audio/c', 1700000000);

    -- user-ms-1 has two media rows for the track; the newer verified_at
    -- (128 kbps / 2 MB) must win. user-ms-2's even newer row
    -- (320 kbps / 3 MB) belongs to a different caller and must never
    -- surface for user-ms-1.
    INSERT INTO track_media (user_id, track_id, source_job_id, content_hash, format, bitrate, verified_at)
    VALUES ('user-ms-1', '22222222-2222-2222-2222-222222222222', 'job_ms1', repeat('a', 64), 'opus', 64, 1700000100),
           ('user-ms-1', '22222222-2222-2222-2222-222222222222', 'job_ms1', repeat('b', 64), 'flac', 128, 1700000200),
           ('user-ms-2', '22222222-2222-2222-2222-222222222222', 'job_ms2', repeat('c', 64), 'flac', 320, 1700000300);

    -- Full ladder for user-ms-1.
    SELECT track_duration_ms, track_mbid, bitrate_kbps, file_bytes
    INTO got_track_ms, got_mbid, got_bitrate, got_bytes
    FROM pg_temp.meter_meta('22222222-2222-2222-2222-222222222222', 'user-ms-1');
    IF NOT FOUND OR got_track_ms IS DISTINCT FROM 240000 OR got_mbid IS DISTINCT FROM '33333333-3333-3333-3333-333333333333'
       OR got_bitrate IS DISTINCT FROM 128 OR got_bytes IS DISTINCT FROM 2000000 THEN
        RAISE EXCEPTION 'full-ladder meta wrong: track=% mb=% kbps=% bytes=%',
            got_track_ms, got_mbid, got_bitrate, got_bytes;
    END IF;

    -- The separate mirror query resolves the recording key, not the local id.
    SELECT mb_duration_ms INTO got_mb_ms FROM pg_temp.meter_mirror(got_mbid);
    IF NOT FOUND OR got_mb_ms IS DISTINCT FROM 241000 THEN
        RAISE EXCEPTION 'mirror duration wrong: %', got_mb_ms;
    END IF;
    SELECT count(*) INTO row_count
    FROM pg_temp.meter_mirror('22222222-2222-2222-2222-222222222222');
    IF row_count <> 0 THEN
        RAISE EXCEPTION 'local id unexpectedly matched mirror: % rows', row_count;
    END IF;

    -- user-ms-2 sees their own newest media.
    SELECT bitrate_kbps, file_bytes INTO got_bitrate, got_bytes
    FROM pg_temp.meter_meta('22222222-2222-2222-2222-222222222222', 'user-ms-2');
    IF NOT FOUND OR got_bitrate IS DISTINCT FROM 320 OR got_bytes IS DISTINCT FROM 3000000 THEN
        RAISE EXCEPTION 'second-user media rungs wrong: kbps=% bytes=%', got_bitrate, got_bytes;
    END IF;

    -- A user with no media still gets the duration rungs, NULL below.
    SELECT track_duration_ms, track_mbid, bitrate_kbps, file_bytes
    INTO got_track_ms, got_mbid, got_bitrate, got_bytes
    FROM pg_temp.meter_meta('22222222-2222-2222-2222-222222222222', 'user-ms-3');
    IF NOT FOUND OR got_track_ms IS DISTINCT FROM 240000 OR got_mbid IS DISTINCT FROM '33333333-3333-3333-3333-333333333333'
       OR got_bitrate IS NOT NULL OR got_bytes IS NOT NULL THEN
        RAISE EXCEPTION 'no-media meta wrong: track=% mb=% kbps=% bytes=%',
            got_track_ms, got_mbid, got_bitrate, got_bytes;
    END IF;

    -- The bare track (NULL duration, no mbid, no media): one row, all NULL.
    SELECT track_duration_ms, track_mbid, bitrate_kbps, file_bytes
    INTO got_track_ms, got_mbid, got_bitrate, got_bytes
    FROM pg_temp.meter_meta('55555555-5555-5555-5555-555555555555', 'user-ms-1');
    IF NOT FOUND OR got_track_ms IS NOT NULL OR got_mbid IS NOT NULL
       OR got_bitrate IS NOT NULL OR got_bytes IS NOT NULL THEN
        RAISE EXCEPTION 'bare-track meta wrong: track=% mb=% kbps=% bytes=%',
            got_track_ms, got_mbid, got_bitrate, got_bytes;
    END IF;

    -- A unique recording resolves the artifact without ambiguity.
    SELECT track_duration_ms, track_mbid, bitrate_kbps, file_bytes, occurrence_ambiguous
    INTO got_track_ms, got_mbid, got_bitrate, got_bytes, ambiguous
    FROM pg_temp.meter_meta('33333333-3333-3333-3333-333333333333', 'user-ms-1');
    IF NOT FOUND OR got_track_ms IS DISTINCT FROM 240000
       OR got_mbid IS DISTINCT FROM '33333333-3333-3333-3333-333333333333'
       OR got_bitrate IS DISTINCT FROM 128 OR got_bytes IS DISTINCT FROM 2000000
       OR ambiguous IS DISTINCT FROM FALSE THEN
        RAISE EXCEPTION 'unique recording metadata incorrect';
    END IF;

    -- Insert the larger occurrence first; a recording-only lookup must be
    -- marked ambiguous irrespective of insertion order.
    INSERT INTO tracks (id, mbid, release_id, title, medium_no, track_no, duration_ms)
    VALUES ('bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb', 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa',
            '44444444-4444-4444-4444-444444444444', 'Later occurrence', 1, 3, 240000),
           ('99999999-9999-9999-9999-999999999999', 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa',
            '44444444-4444-4444-4444-444444444444', 'Earlier occurrence', 1, 4, 60000);
    SELECT occurrence_ambiguous INTO ambiguous
    FROM pg_temp.meter_meta('aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa', 'user-ms-1');
    IF NOT FOUND OR ambiguous IS DISTINCT FROM TRUE THEN
        RAISE EXCEPTION 'shared recording was not marked ambiguous';
    END IF;
    SELECT track_duration_ms, occurrence_ambiguous INTO got_track_ms, ambiguous
    FROM pg_temp.meter_meta('99999999-9999-9999-9999-999999999999', 'user-ms-1');
    IF NOT FOUND OR got_track_ms IS DISTINCT FROM 60000 OR ambiguous IS DISTINCT FROM FALSE THEN
        RAISE EXCEPTION 'first exact occurrence did not retain its identity';
    END IF;
    SELECT track_duration_ms, occurrence_ambiguous INTO got_track_ms, ambiguous
    FROM pg_temp.meter_meta('bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb', 'user-ms-1');
    IF NOT FOUND OR got_track_ms IS DISTINCT FROM 240000 OR ambiguous IS DISTINCT FROM FALSE THEN
        RAISE EXCEPTION 'second exact occurrence did not retain its identity';
    END IF;

    -- A local ID takes precedence over a different track's recording MBID.
    INSERT INTO tracks (id, mbid, release_id, title, medium_no, track_no, duration_ms)
    VALUES ('cccccccc-cccc-cccc-cccc-cccccccccccc', '22222222-2222-2222-2222-222222222222',
            '44444444-4444-4444-4444-444444444444', 'Collision', 1, 5, 999000);
    SELECT track_duration_ms, track_mbid, occurrence_ambiguous INTO got_track_ms, got_mbid, ambiguous
    FROM pg_temp.meter_meta('22222222-2222-2222-2222-222222222222', 'user-ms-1');
    IF NOT FOUND OR got_track_ms IS DISTINCT FROM 240000
       OR got_mbid IS DISTINCT FROM '33333333-3333-3333-3333-333333333333'
       OR ambiguous IS DISTINCT FROM FALSE THEN
        RAISE EXCEPTION 'local ID lost to recording namespace collision';
    END IF;

    INSERT INTO mb_recording (mbid, title, duration_ms)
    VALUES ('dddddddd-dddd-dddd-dddd-dddddddddddd', 'Orphan recording', 61000);
    SELECT count(*) INTO row_count
    FROM pg_temp.meter_meta('dddddddd-dddd-dddd-dddd-dddddddddddd', 'user-ms-1');
    IF row_count <> 0 THEN
        RAISE EXCEPTION 'orphan recording unexpectedly matched an artifact';
    END IF;
    SELECT mb_duration_ms INTO got_mb_ms
    FROM pg_temp.meter_mirror('dddddddd-dddd-dddd-dddd-dddddddddddd');
    IF NOT FOUND OR got_mb_ms IS DISTINCT FROM 61000 THEN
        RAISE EXCEPTION 'orphan recording mirror duration wrong: %', got_mb_ms;
    END IF;
    SELECT count(*) INTO row_count
    FROM pg_temp.meter_mirror('66666666-6666-6666-6666-666666666666');
    IF row_count <> 0 THEN
        RAISE EXCEPTION 'unknown recording matched mirror';
    END IF;

    -- Unknown track: zero rows (the Rust side maps this to the
    -- all-None meta and meters on the fallback rung).
    SELECT count(*) INTO row_count
    FROM pg_temp.meter_meta('66666666-6666-6666-6666-666666666666', 'user-ms-1');
    IF row_count <> 0 THEN
        RAISE EXCEPTION 'unknown track matched % rows', row_count;
    END IF;
END
$$;

ROLLBACK;
