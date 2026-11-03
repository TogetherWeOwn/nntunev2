-- Regression test for the tracks + track_sources writers
-- (workers/ingest/src/track_writers.rs against migrations 0022/0027).
-- Run after ALL migrations are applied (see ../run_migration_tests.sh).
-- Everything happens inside one transaction that is rolled back, so it
-- leaves no rows; any broken expectation RAISEs and (with ON_ERROR_STOP)
-- fails the run.
--
-- track_sources is global (migration 0027): one provenance slot per
-- track_id, no per-user owner. The only users row seeded is the owner the
-- ingest_jobs FK needs; jobs (not users) are the writer fence. This test
-- executes both writer statements in their exact shapes (literals stand in
-- for the $n parameters): the fenced tracks insert converging on the
-- (release_id, medium_no, track_no) natural key with a DB-owned surrogate
-- id, and the fenced track_sources insert converging on the (track_id)
-- slot with the v2 envelope literal — plus the PK re-selects the consumer
-- uses to separate a benign replay from a fenced job, and the negatives
-- (fence, FKs, envelope CHECKs, the NOT NULL release that unmatched jobs
-- must never smuggle).

BEGIN;

DO $$
DECLARE
    row_count_var INTEGER;
    track_id_var UUID;
    source_job_var TEXT;
    track_title_var TEXT;
BEGIN
    -- The single users row exists only for the ingest_jobs owner FK; the
    -- writers never bind it (no per-user owner anywhere below).
    INSERT INTO users (id, email, created_at)
    VALUES ('user-tw-1', 'tw-1@example.com', 1700000000);

    INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
    VALUES ('job_01trackwriters0001', 'user-tw-1',
            'ingest:user-tw-1:9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734:upload',
            1700000000, 'done'),
           ('job_01trackwriters0002', 'user-tw-1',
            'ingest:user-tw-1:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:upload',
            1700000000, 'done');

    -- Statement 1: tracks. The surrogate id is DB-owned (gen_random_uuid),
    -- duration_ms stays NULL (no probe stage), and the fence job ($6) is
    -- bound but not inserted: it gates the write without becoming a column.
    INSERT INTO tracks (mbid, release_id, title, medium_no, track_no)
    SELECT '11111111-1111-4111-8111-111111111111',
           'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa',
           'First', 1, 1
    FROM (SELECT id FROM ingest_jobs WHERE id = 'job_01trackwriters0001' FOR SHARE) AS writer_fence
    ON CONFLICT (release_id, medium_no, track_no) DO NOTHING;
    SELECT count(*) INTO row_count_var FROM tracks
    WHERE release_id = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa'
      AND medium_no = 1 AND track_no = 1
      AND id IS NOT NULL AND duration_ms IS NULL;
    IF row_count_var <> 1 THEN
        RAISE EXCEPTION 'tracks insert did not land one row with defaulted id and NULL duration';
    END IF;

    -- Idempotent replay converges on the natural key (zero further rows).
    INSERT INTO tracks (mbid, release_id, title, medium_no, track_no)
    SELECT '11111111-1111-4111-8111-111111111111',
           'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa',
           'First', 1, 1
    FROM (SELECT id FROM ingest_jobs WHERE id = 'job_01trackwriters0001' FOR SHARE) AS writer_fence
    ON CONFLICT (release_id, medium_no, track_no) DO NOTHING;
    IF (SELECT count(*) FROM tracks WHERE release_id = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa') <> 1 THEN
        RAISE EXCEPTION 'tracks replay did not converge on the natural key';
    END IF;

    -- First writer wins: a divergent later write for one position must not
    -- flap the shared cross-user row.
    INSERT INTO tracks (mbid, release_id, title, medium_no, track_no)
    SELECT '22222222-2222-4222-8222-222222222222',
           'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa',
           'Renamed', 1, 1
    FROM (SELECT id FROM ingest_jobs WHERE id = 'job_01trackwriters0001' FOR SHARE) AS writer_fence
    ON CONFLICT (release_id, medium_no, track_no) DO NOTHING;
    SELECT title INTO track_title_var FROM tracks
    WHERE release_id = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa' AND medium_no = 1 AND track_no = 1;
    IF NOT FOUND OR track_title_var <> 'First' THEN
        RAISE EXCEPTION 'tracks conflict overwrote the first writer''s shared row';
    END IF;

    -- PK re-select (track_writers.rs select_track_id_by_position_sql shape):
    -- the consumer separates a benign replay from a fenced job by whether
    -- this finds the row, and learns the DB-owned id for the downstream
    -- FKs.
    SELECT id INTO track_id_var FROM tracks
    WHERE release_id = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa' AND medium_no = 1 AND track_no = 1;
    IF NOT FOUND OR track_id_var IS NULL THEN
        RAISE EXCEPTION 'tracks PK re-select missed the written row';
    END IF;

    -- Publishing-job fence: an insert naming a job row that was never
    -- persisted lands zero rows. A fresh position isolates the fence from
    -- the conflict target (a replay of the position above would also land
    -- zero rows through DO NOTHING).
    INSERT INTO tracks (mbid, release_id, title, medium_no, track_no)
    SELECT '33333333-3333-4333-8333-333333333333',
           'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb',
           'Fenced', 1, 1
    FROM (SELECT id FROM ingest_jobs WHERE id = 'job_missing' FOR SHARE) AS writer_fence
    ON CONFLICT (release_id, medium_no, track_no) DO NOTHING;
    IF (SELECT count(*) FROM tracks WHERE release_id = 'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb') <> 0 THEN
        RAISE EXCEPTION 'fenced tracks insert landed a row for a missing job';
    END IF;

    -- Unmatched jobs have no row to write: release_id is NOT NULL, so a
    -- NULL smuggle fails at the schema instead of landing a releaseless
    -- track (the writer skips with reason before ever binding this).
    BEGIN
        INSERT INTO tracks (mbid, release_id, title, medium_no, track_no)
        SELECT '55555555-5555-4333-8333-555555555555',
               NULL,
               'No Release', 1, 1
        FROM (SELECT id FROM ingest_jobs WHERE id = 'job_01trackwriters0001' FOR SHARE) AS writer_fence
        ON CONFLICT (release_id, medium_no, track_no) DO NOTHING;
        RAISE EXCEPTION 'tracks accepted a NULL release_id smuggle';
    EXCEPTION WHEN not_null_violation THEN
        NULL;
    END;

    -- Statement 2: track_sources. The envelope version is the 2 literal
    -- (only v2 mint output binds); indexer_id is NULL until the indexers
    -- table lands (the pipeline's indexer_ref is a vault handle, not a
    -- UUID, so there is no id to store). The fence holds the publishing
    -- job ($2); the same column is inserted as source_job_id.
    INSERT INTO track_sources
        (track_id, source_job_id, indexer_id, nzb_ref_encrypted,
         nzb_ref_envelope_version, nzb_ref_kek_version,
         nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped,
         nzb_ref_payload_nonce, nzb_ref_payload_format)
    SELECT track_id_var,
           'job_01trackwriters0001', NULL, '\x0304'::bytea,
           2, 1,
           '\x000000000000000000000000'::bytea,
           '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
           '\x000000000000000000000000'::bytea,
           1
    FROM (SELECT id FROM ingest_jobs WHERE id = 'job_01trackwriters0001' FOR SHARE) AS writer_fence
    ON CONFLICT (track_id) DO NOTHING;
    SELECT count(*) INTO row_count_var FROM track_sources
    WHERE track_id = track_id_var;
    IF row_count_var <> 1 THEN
        RAISE EXCEPTION 'track_sources insert did not land one global provenance row';
    END IF;

    -- Idempotent replay converges, and a second job's source for the same
    -- track does nothing (first wins): the slot is one row per track.
    INSERT INTO track_sources
        (track_id, source_job_id, indexer_id, nzb_ref_encrypted,
         nzb_ref_envelope_version, nzb_ref_kek_version,
         nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped,
         nzb_ref_payload_nonce, nzb_ref_payload_format)
    SELECT track_id_var,
           'job_01trackwriters0001', NULL, '\x0304'::bytea,
           2, 1,
           '\x000000000000000000000000'::bytea,
           '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
           '\x000000000000000000000000'::bytea,
           1
    FROM (SELECT id FROM ingest_jobs WHERE id = 'job_01trackwriters0001' FOR SHARE) AS writer_fence
    ON CONFLICT (track_id) DO NOTHING;
    INSERT INTO track_sources
        (track_id, source_job_id, indexer_id, nzb_ref_encrypted,
         nzb_ref_envelope_version, nzb_ref_kek_version,
         nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped,
         nzb_ref_payload_nonce, nzb_ref_payload_format)
    SELECT track_id_var,
           'job_01trackwriters0002', NULL, '\x0506'::bytea,
           2, 1,
           '\x000000000000000000000000'::bytea,
           '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
           '\x000000000000000000000000'::bytea,
           1
    FROM (SELECT id FROM ingest_jobs WHERE id = 'job_01trackwriters0002' FOR SHARE) AS writer_fence
    ON CONFLICT (track_id) DO NOTHING;
    SELECT source_job_id INTO source_job_var FROM track_sources
    WHERE track_id = track_id_var;
    IF NOT FOUND OR source_job_var <> 'job_01trackwriters0001' THEN
        RAISE EXCEPTION 'track_sources conflict did not keep the first provenance';
    END IF;
    IF (SELECT count(*) FROM track_sources WHERE track_id = track_id_var) <> 1 THEN
        RAISE EXCEPTION 'track_sources replay did not converge on one slot row';
    END IF;

    -- PK re-select (track_writers.rs select_track_source_by_pk_sql shape).
    PERFORM 1 FROM track_sources
    WHERE track_id = track_id_var;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'track_sources PK re-select missed the written row';
    END IF;

    -- indexer_id binds a UUID once issued (FK-deferred: the indexers table
    -- does not exist yet, so no constraint fires — the column only stores).
    INSERT INTO tracks (mbid, release_id, title, medium_no, track_no)
    SELECT '66666666-6666-4333-8333-666666666666',
           'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa',
           'Second', 1, 2
    FROM (SELECT id FROM ingest_jobs WHERE id = 'job_01trackwriters0001' FOR SHARE) AS writer_fence
    ON CONFLICT (release_id, medium_no, track_no) DO NOTHING;
    INSERT INTO track_sources
        (track_id, source_job_id, indexer_id, nzb_ref_encrypted,
         nzb_ref_envelope_version, nzb_ref_kek_version,
         nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped,
         nzb_ref_payload_nonce, nzb_ref_payload_format)
    SELECT (SELECT id FROM tracks WHERE release_id = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa' AND medium_no = 1 AND track_no = 2),
           'job_01trackwriters0001', 'dddddddd-dddd-dddd-dddd-dddddddddddd', '\x0304'::bytea,
           2, 1,
           '\x000000000000000000000000'::bytea,
           '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
           '\x000000000000000000000000'::bytea,
           1
    FROM (SELECT id FROM ingest_jobs WHERE id = 'job_01trackwriters0001' FOR SHARE) AS writer_fence
    ON CONFLICT (track_id) DO NOTHING;
    IF (SELECT count(*) FROM track_sources
        WHERE indexer_id = 'dddddddd-dddd-dddd-dddd-dddddddddddd') <> 1 THEN
        RAISE EXCEPTION 'track_sources did not store the deferred indexer UUID';
    END IF;

    -- A third track so the remaining negatives each get a fresh slot.
    INSERT INTO tracks (mbid, release_id, title, medium_no, track_no)
    SELECT '77777777-7777-4333-8333-777777777777',
           'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa',
           'Third', 1, 3
    FROM (SELECT id FROM ingest_jobs WHERE id = 'job_01trackwriters0001' FOR SHARE) AS writer_fence
    ON CONFLICT (release_id, medium_no, track_no) DO NOTHING;

    -- A missing job row fences the write to zero rows (no row exists for
    -- this track yet, so zero rows isolate the fence — not a conflict).
    INSERT INTO track_sources
        (track_id, source_job_id, indexer_id, nzb_ref_encrypted,
         nzb_ref_envelope_version, nzb_ref_kek_version,
         nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped,
         nzb_ref_payload_nonce, nzb_ref_payload_format)
    SELECT (SELECT id FROM tracks WHERE release_id = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa' AND medium_no = 1 AND track_no = 3),
           'job_missing', NULL, '\x0304'::bytea,
           2, 1,
           '\x000000000000000000000000'::bytea,
           '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
           '\x000000000000000000000000'::bytea,
           1
    FROM (SELECT id FROM ingest_jobs WHERE id = 'job_missing' FOR SHARE) AS writer_fence
    ON CONFLICT (track_id) DO NOTHING;
    IF (SELECT count(*) FROM track_sources
        WHERE track_id = (SELECT id FROM tracks WHERE release_id = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa' AND medium_no = 1 AND track_no = 3)) <> 0 THEN
        RAISE EXCEPTION 'fenced track_sources insert landed a row for a missing job';
    END IF;

    -- Sources cannot reference a missing track. The slot is fresh
    -- (ON CONFLICT DO NOTHING on a taken slot would converge instead of
    -- reaching the FK).
    BEGIN
        INSERT INTO track_sources
            (track_id, source_job_id, indexer_id, nzb_ref_encrypted,
             nzb_ref_envelope_version, nzb_ref_kek_version,
             nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped,
             nzb_ref_payload_nonce, nzb_ref_payload_format)
        SELECT 'eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee',
               'job_01trackwriters0001', NULL, '\x0304'::bytea,
               2, 1,
               '\x000000000000000000000000'::bytea,
               '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
               '\x000000000000000000000000'::bytea,
               1
        FROM (SELECT id FROM ingest_jobs WHERE id = 'job_01trackwriters0001' FOR SHARE) AS writer_fence
        ON CONFLICT (track_id) DO NOTHING;
        RAISE EXCEPTION 'FK accepted a source for a missing track';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;
    END;

    -- Envelope guards: a short nonce is rejected at the write boundary even
    -- when the fence is open (the Rust validator fails these first; the
    -- CHECK backstops callers that skipped it). Fresh slot, so the CHECK —
    -- not the conflict target — decides.
    BEGIN
        INSERT INTO track_sources
            (track_id, source_job_id, indexer_id, nzb_ref_encrypted,
             nzb_ref_envelope_version, nzb_ref_kek_version,
             nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped,
             nzb_ref_payload_nonce, nzb_ref_payload_format)
        SELECT (SELECT id FROM tracks WHERE release_id = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa' AND medium_no = 1 AND track_no = 3),
               'job_01trackwriters0001', NULL, '\x0304'::bytea,
               2, 1,
               '\x00'::bytea,
               '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
               '\x000000000000000000000000'::bytea,
               1
        FROM (SELECT id FROM ingest_jobs WHERE id = 'job_01trackwriters0001' FOR SHARE) AS writer_fence
        ON CONFLICT (track_id) DO NOTHING;
        RAISE EXCEPTION 'CHECK accepted a short nzb_ref wrap nonce';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
END $$;

ROLLBACK;
