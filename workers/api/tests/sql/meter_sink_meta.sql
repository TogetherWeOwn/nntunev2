-- Regression test for the meter-sink metadata statements
-- (workers/api/src/meter_sink.rs `track_meter_meta_app_statement` and
-- `track_meter_meta_mirror_statement`). Run after ALL migrations are
-- applied (see ../run_migration_tests.sh). Everything happens inside
-- one transaction that is rolled back, so it leaves no rows; any
-- broken expectation RAISEs and (with ON_ERROR_STOP) fails the run.
--
-- The two statements are pinned exactly as the Rust side runs them
-- with literals standing in for the $n parameters (track id as UUID
-- text, user id). Production runs them separately — the app query on
-- the app connection (which has no mb_recording), the mirror query
-- on the mirror connection — so this fixture asserts the same
-- projections with the literals below. It pins: the app statement
-- resolves the track by local id OR recording MBID (the playback-id
-- namespace) with the local id winning collisions; the media rungs
-- pick the CALLER's most recently verified media row (another user's
-- newer row must not leak in); a track with no durations or media
-- still yields its one row with NULLs; an unknown id yields no app
-- row and the mirror probe alone supplies the recording rung;
-- unknown recording yields no mirror row (the fallback rung meters
-- it, never an error). A recording MBID shared by several local
-- occurrences reports occurrence_ambiguous (the Rust side then
-- claims no artifact rung); a single occurrence or an exact local
-- id does not.

BEGIN;

DO $$
DECLARE
    got_track_ms INTEGER;
    got_mbid TEXT;
    got_bitrate INTEGER;
    got_bytes BIGINT;
    got_ambiguous BOOLEAN;
    got_mb_ms INTEGER;
    row_count INTEGER;
BEGIN
    INSERT INTO users (id, email, created_at)
    VALUES ('user-ms-1', 'ms-1@example.com', 1700000000),
           ('user-ms-2', 'ms-2@example.com', 1700000000),
           ('user-ms-3', 'ms-3@example.com', 1700000000);

    INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
    VALUES ('job_ms1', 'user-ms-1', 'ms-key-1', 1700000000, 'done'),
           ('job_ms2', 'user-ms-2', 'ms-key-2', 1700000000, 'done');

    INSERT INTO mb_recording (mbid, title, duration_ms)
    VALUES ('33333333-3333-3333-3333-333333333333', 'Meter Track', 241000),
           ('77777777-7777-7777-7777-777777777777', 'Unartifacts Track', 61000);

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

    -- Full app ladder for user-ms-1 by LOCAL id: the exact Rust app
    -- statement, literals inlined for $1 (track id) and $2 (user id).
    SELECT t.duration_ms AS track_duration_ms, t.mbid::text AS track_mbid,
     (SELECT m.bitrate FROM track_media m WHERE m.track_id = t.id AND m.user_id = 'user-ms-1'
     ORDER BY m.verified_at DESC, m.content_hash ASC LIMIT 1) AS bitrate_kbps,
     (SELECT c.byte_size FROM track_media m JOIN cache_objects c
     ON c.content_hash = m.content_hash WHERE m.track_id = t.id AND m.user_id = 'user-ms-1'
     ORDER BY m.verified_at DESC, m.content_hash ASC LIMIT 1) AS file_bytes,
     (t.id <> '22222222-2222-2222-2222-222222222222'::uuid
      AND (SELECT COUNT(*) FROM tracks t2 WHERE t2.mbid = '22222222-2222-2222-2222-222222222222'::uuid) > 1)
     AS occurrence_ambiguous
     INTO got_track_ms, got_mbid, got_bitrate, got_bytes, got_ambiguous
     FROM tracks t WHERE t.id = '22222222-2222-2222-2222-222222222222'::uuid
      OR t.mbid = '22222222-2222-2222-2222-222222222222'::uuid
     ORDER BY (t.id = '22222222-2222-2222-2222-222222222222'::uuid) DESC, t.id LIMIT 1;
    IF NOT FOUND OR got_track_ms <> 240000
       OR got_mbid <> '33333333-3333-3333-3333-333333333333'
       OR got_bitrate <> 128 OR got_bytes <> 2000000 OR got_ambiguous THEN
        RAISE EXCEPTION 'full-ladder app meta wrong: track=% mbid=% kbps=% bytes=% ambiguous=%',
            got_track_ms, got_mbid, got_bitrate, got_bytes, got_ambiguous;
    END IF;

    -- The same app row resolves by RECORDING MBID (the catalogue
    -- playback id): identical media rungs, keyed off the artifact row.
    SELECT t.duration_ms AS track_duration_ms, t.mbid::text AS track_mbid,
     (SELECT m.bitrate FROM track_media m WHERE m.track_id = t.id AND m.user_id = 'user-ms-1'
     ORDER BY m.verified_at DESC, m.content_hash ASC LIMIT 1) AS bitrate_kbps,
     (SELECT c.byte_size FROM track_media m JOIN cache_objects c
     ON c.content_hash = m.content_hash WHERE m.track_id = t.id AND m.user_id = 'user-ms-1'
     ORDER BY m.verified_at DESC, m.content_hash ASC LIMIT 1) AS file_bytes,
     (t.id <> '33333333-3333-3333-3333-333333333333'::uuid
      AND (SELECT COUNT(*) FROM tracks t2 WHERE t2.mbid = '33333333-3333-3333-3333-333333333333'::uuid) > 1)
     AS occurrence_ambiguous
     INTO got_track_ms, got_mbid, got_bitrate, got_bytes, got_ambiguous
     FROM tracks t WHERE t.id = '33333333-3333-3333-3333-333333333333'::uuid
      OR t.mbid = '33333333-3333-3333-3333-333333333333'::uuid
     ORDER BY (t.id = '33333333-3333-3333-3333-333333333333'::uuid) DESC, t.id LIMIT 1;
    IF NOT FOUND OR got_track_ms <> 240000
       OR got_mbid <> '33333333-3333-3333-3333-333333333333'
       OR got_bitrate <> 128 OR got_bytes <> 2000000 OR got_ambiguous THEN
        RAISE EXCEPTION 'recording-id app meta wrong: track=% mbid=% kbps=% bytes=% ambiguous=%',
            got_track_ms, got_mbid, got_bitrate, got_bytes, got_ambiguous;
    END IF;

    -- Mirror rung for the resolved recording MBID.
    SELECT duration_ms AS mb_duration_ms INTO got_mb_ms
     FROM mb_recording WHERE mbid = '33333333-3333-3333-3333-333333333333'::uuid;
    IF NOT FOUND OR got_mb_ms <> 241000 THEN
        RAISE EXCEPTION 'mirror rung wrong: %', got_mb_ms;
    END IF;

    -- Mirror rung with NO artifact row: the playback id probes the
    -- mirror directly and still resolves the known short duration.
    SELECT duration_ms AS mb_duration_ms INTO got_mb_ms
     FROM mb_recording WHERE mbid = '77777777-7777-7777-7777-777777777777'::uuid;
    IF NOT FOUND OR got_mb_ms <> 61000 THEN
        RAISE EXCEPTION 'orphan-recording mirror rung wrong: %', got_mb_ms;
    END IF;
    SELECT count(*) INTO row_count
     FROM tracks t WHERE t.id = '77777777-7777-7777-7777-777777777777'::uuid
      OR t.mbid = '77777777-7777-7777-7777-777777777777'::uuid;
    IF row_count <> 0 THEN
        RAISE EXCEPTION 'orphan recording unexpectedly matched % app rows', row_count;
    END IF;

    -- Unknown recording: zero mirror rows (the Rust side maps this to
    -- no mb_duration_ms and meters on the fallback rung).
    SELECT count(*) INTO row_count
     FROM mb_recording WHERE mbid = '66666666-6666-6666-6666-666666666666'::uuid;
    IF row_count <> 0 THEN
        RAISE EXCEPTION 'unknown recording matched % rows', row_count;
    END IF;

    -- user-ms-2 sees their own newest media.
    SELECT (SELECT m.bitrate FROM track_media m WHERE m.track_id = t.id AND m.user_id = 'user-ms-2'
            ORDER BY m.verified_at DESC, m.content_hash ASC LIMIT 1),
           (SELECT c.byte_size FROM track_media m JOIN cache_objects c
            ON c.content_hash = m.content_hash WHERE m.track_id = t.id AND m.user_id = 'user-ms-2'
            ORDER BY m.verified_at DESC, m.content_hash ASC LIMIT 1)
    INTO got_bitrate, got_bytes
    FROM tracks t WHERE t.id = '22222222-2222-2222-2222-222222222222'::uuid;
    IF got_bitrate <> 320 OR got_bytes <> 3000000 THEN
        RAISE EXCEPTION 'second-user media rungs wrong: kbps=% bytes=%', got_bitrate, got_bytes;
    END IF;

    -- A user with no media still gets the duration rungs, NULL below.
    SELECT t.duration_ms AS track_duration_ms, t.mbid::text AS track_mbid,
     (SELECT m.bitrate FROM track_media m WHERE m.track_id = t.id AND m.user_id = 'user-ms-3'
     ORDER BY m.verified_at DESC, m.content_hash ASC LIMIT 1) AS bitrate_kbps,
     (SELECT c.byte_size FROM track_media m JOIN cache_objects c
     ON c.content_hash = m.content_hash WHERE m.track_id = t.id AND m.user_id = 'user-ms-3'
     ORDER BY m.verified_at DESC, m.content_hash ASC LIMIT 1) AS file_bytes,
     (t.id <> '22222222-2222-2222-2222-222222222222'::uuid
      AND (SELECT COUNT(*) FROM tracks t2 WHERE t2.mbid = '22222222-2222-2222-2222-222222222222'::uuid) > 1)
     AS occurrence_ambiguous
     INTO got_track_ms, got_mbid, got_bitrate, got_bytes, got_ambiguous
     FROM tracks t WHERE t.id = '22222222-2222-2222-2222-222222222222'::uuid
      OR t.mbid = '22222222-2222-2222-2222-222222222222'::uuid
     ORDER BY (t.id = '22222222-2222-2222-2222-222222222222'::uuid) DESC, t.id LIMIT 1;
    IF NOT FOUND OR got_track_ms <> 240000
       OR got_mbid <> '33333333-3333-3333-3333-333333333333'
       OR got_bitrate IS NOT NULL OR got_bytes IS NOT NULL THEN
        RAISE EXCEPTION 'no-media meta wrong: track=% mbid=% kbps=% bytes=%',
            got_track_ms, got_mbid, got_bitrate, got_bytes;
    END IF;

    -- The bare track (NULL duration, no mbid, no media): one row, all NULL.
    SELECT t.duration_ms AS track_duration_ms, t.mbid::text AS track_mbid,
     (SELECT m.bitrate FROM track_media m WHERE m.track_id = t.id AND m.user_id = 'user-ms-1'
     ORDER BY m.verified_at DESC, m.content_hash ASC LIMIT 1) AS bitrate_kbps,
     (SELECT c.byte_size FROM track_media m JOIN cache_objects c
     ON c.content_hash = m.content_hash WHERE m.track_id = t.id AND m.user_id = 'user-ms-1'
     ORDER BY m.verified_at DESC, m.content_hash ASC LIMIT 1) AS file_bytes,
     (t.id <> '55555555-5555-5555-5555-555555555555'::uuid
      AND (SELECT COUNT(*) FROM tracks t2 WHERE t2.mbid = '55555555-5555-5555-5555-555555555555'::uuid) > 1)
     AS occurrence_ambiguous
     INTO got_track_ms, got_mbid, got_bitrate, got_bytes, got_ambiguous
     FROM tracks t WHERE t.id = '55555555-5555-5555-5555-555555555555'::uuid
      OR t.mbid = '55555555-5555-5555-5555-555555555555'::uuid
     ORDER BY (t.id = '55555555-5555-5555-5555-555555555555'::uuid) DESC, t.id LIMIT 1;
    IF NOT FOUND OR got_track_ms IS NOT NULL OR got_mbid IS NOT NULL
       OR got_bitrate IS NOT NULL OR got_bytes IS NOT NULL THEN
        RAISE EXCEPTION 'bare-track meta wrong: track=% mbid=% kbps=% bytes=%',
            got_track_ms, got_mbid, got_bitrate, got_bytes;
    END IF;

    -- Two occurrences sharing one recording MBID: a recording-only
    -- match is ambiguous (the Rust side must claim neither
    -- occurrence's artifact rungs, only the mirror rung), while each
    -- exact local id keeps its own occurrence metadata. They occupy
    -- fresh track positions (1,3)/(1,4) on the release, never (1,1):
    -- the schema enforces UNIQUE (release_id, medium_no, track_no)
    -- and (1,1) already holds the Meter Track. Insert the larger
    -- local id first so the outcome cannot ride on order.
    INSERT INTO tracks (id, mbid, release_id, title, medium_no, track_no, duration_ms)
    VALUES ('88888888-8888-8888-8888-888888888888',
            '99999999-9999-9999-9999-999999999999',
            '44444444-4444-4444-4444-444444444444',
            'Shared B', 1, 3, 240000),
           ('11111111-1111-1111-1111-111111111111',
            '99999999-9999-9999-9999-999999999999',
            '44444444-4444-4444-4444-444444444444',
            'Shared A', 1, 4, 60000);
    INSERT INTO mb_recording (mbid, title, duration_ms)
    VALUES ('99999999-9999-9999-9999-999999999999', 'Shared', 241000);
    SELECT t.duration_ms AS track_duration_ms, t.mbid::text AS track_mbid,
     (SELECT m.bitrate FROM track_media m WHERE m.track_id = t.id AND m.user_id = 'user-ms-1'
     ORDER BY m.verified_at DESC, m.content_hash ASC LIMIT 1) AS bitrate_kbps,
     (SELECT c.byte_size FROM track_media m JOIN cache_objects c
     ON c.content_hash = m.content_hash WHERE m.track_id = t.id AND m.user_id = 'user-ms-1'
     ORDER BY m.verified_at DESC, m.content_hash ASC LIMIT 1) AS file_bytes,
     (t.id <> '99999999-9999-9999-9999-999999999999'::uuid
      AND (SELECT COUNT(*) FROM tracks t2 WHERE t2.mbid = '99999999-9999-9999-9999-999999999999'::uuid) > 1)
     AS occurrence_ambiguous
     INTO got_track_ms, got_mbid, got_bitrate, got_bytes, got_ambiguous
     FROM tracks t WHERE t.id = '99999999-9999-9999-9999-999999999999'::uuid
      OR t.mbid = '99999999-9999-9999-9999-999999999999'::uuid
     ORDER BY (t.id = '99999999-9999-9999-9999-999999999999'::uuid) DESC, t.id LIMIT 1;
    IF NOT FOUND OR NOT got_ambiguous
       OR got_mbid <> '99999999-9999-9999-9999-999999999999' THEN
        RAISE EXCEPTION 'shared-recording ambiguity wrong: mbid=% ambiguous=%',
            got_mbid, got_ambiguous;
    END IF;
    SELECT duration_ms AS mb_duration_ms INTO got_mb_ms
     FROM mb_recording WHERE mbid = '99999999-9999-9999-9999-999999999999'::uuid;
    IF NOT FOUND OR got_mb_ms <> 241000 THEN
        RAISE EXCEPTION 'shared-recording mirror rung wrong: %', got_mb_ms;
    END IF;
    SELECT t.duration_ms AS track_duration_ms, t.mbid::text AS track_mbid,
     (SELECT m.bitrate FROM track_media m WHERE m.track_id = t.id AND m.user_id = 'user-ms-1'
     ORDER BY m.verified_at DESC, m.content_hash ASC LIMIT 1) AS bitrate_kbps,
     (SELECT c.byte_size FROM track_media m JOIN cache_objects c
     ON c.content_hash = m.content_hash WHERE m.track_id = t.id AND m.user_id = 'user-ms-1'
     ORDER BY m.verified_at DESC, m.content_hash ASC LIMIT 1) AS file_bytes,
     (t.id <> '88888888-8888-8888-8888-888888888888'::uuid
      AND (SELECT COUNT(*) FROM tracks t2 WHERE t2.mbid = '88888888-8888-8888-8888-888888888888'::uuid) > 1)
     AS occurrence_ambiguous
     INTO got_track_ms, got_mbid, got_bitrate, got_bytes, got_ambiguous
     FROM tracks t WHERE t.id = '88888888-8888-8888-8888-888888888888'::uuid
      OR t.mbid = '88888888-8888-8888-8888-888888888888'::uuid
     ORDER BY (t.id = '88888888-8888-8888-8888-888888888888'::uuid) DESC, t.id LIMIT 1;
    IF NOT FOUND OR got_track_ms <> 240000 OR got_ambiguous THEN
        RAISE EXCEPTION 'shared-recording local-id control wrong: track=% ambiguous=%',
            got_track_ms, got_ambiguous;
    END IF;

    -- Unknown id: zero app rows (the Rust side probes the mirror and
    -- meters on the fallback rung).
    SELECT count(*) INTO row_count
    FROM tracks t WHERE t.id = '66666666-6666-6666-6666-666666666666'::uuid
     OR t.mbid = '66666666-6666-6666-6666-666666666666'::uuid;
    IF row_count <> 0 THEN
        RAISE EXCEPTION 'unknown track matched % rows', row_count;
    END IF;
END
$$;

ROLLBACK;
