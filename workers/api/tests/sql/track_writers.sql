-- Regression test for the tracks + track_sources writers
-- (workers/ingest/src/track_writers.rs against migration 0022). Run after
-- ALL migrations are applied (see ../run_migration_tests.sh). Everything
-- happens inside one transaction that is rolled back, so it leaves no rows;
-- any broken expectation RAISEs and (with ON_ERROR_STOP) fails the run.
--
-- This test executes both writer statements in their exact shapes (literals
-- stand in for the $n parameters): the fenced tracks insert converging on
-- the (release_id, medium_no, track_no) natural key with a DB-owned
-- surrogate id, and the fenced track_sources insert converging on the
-- (user_id, track_id) provenance slot with the v2 envelope literal — plus
-- the PK re-selects the consumer uses to separate a benign replay from a
-- fenced account, and the negatives (fence, FKs, envelope CHECKs, the
-- NOT NULL release that unmatched jobs must never smuggle).

BEGIN;

DO $$
DECLARE
    row_count_var INTEGER;
    track_id_var UUID;
    source_job_var TEXT;
    track_title_var TEXT;
BEGIN
    INSERT INTO users (id, email, created_at)
    VALUES ('user-tw-1', 'tw-1@example.com', 1700000000),
           ('user-tw-2', 'tw-2@example.com', 1700000000),
           ('user-tw-3', 'tw-3@example.com', 1700000000);

    INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
    VALUES ('job_01trackwriters0001', 'user-tw-1',
            'ingest:user-tw-1:9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734:upload',
            1700000000, 'done'),
           ('job_01trackwriters0002', 'user-tw-1',
            'ingest:user-tw-1:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:upload',
            1700000000, 'done');

    -- Statement 1: tracks. The surrogate id is DB-owned (gen_random_uuid),
    -- duration_ms stays NULL (no probe stage), and the fence owner ($6) is
    -- bound but not inserted: it gates the write without becoming a column.
    INSERT INTO tracks (mbid, release_id, title, medium_no, track_no)
    SELECT '11111111-1111-4111-8111-111111111111',
           'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa',
           'First', 1, 1
    FROM (SELECT id FROM users WHERE id = 'user-tw-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence
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
    FROM (SELECT id FROM users WHERE id = 'user-tw-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence
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
    FROM (SELECT id FROM users WHERE id = 'user-tw-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (release_id, medium_no, track_no) DO NOTHING;
    SELECT title INTO track_title_var FROM tracks
    WHERE release_id = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa' AND medium_no = 1 AND track_no = 1;
    IF NOT FOUND OR track_title_var <> 'First' THEN
        RAISE EXCEPTION 'tracks conflict overwrote the first writer''s shared row';
    END IF;

    -- PK re-select (track_writers.rs select_track_id_by_position_sql shape):
    -- the consumer separates a benign replay from a fenced account by
    -- whether this finds the row, and learns the DB-owned id for the
    -- downstream FKs.
    SELECT id INTO track_id_var FROM tracks
    WHERE release_id = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa' AND medium_no = 1 AND track_no = 1;
    IF NOT FOUND OR track_id_var IS NULL THEN
        RAISE EXCEPTION 'tracks PK re-select missed the written row';
    END IF;

    -- Deletion fence (ADR 0002 rule 3): a fenced insert lands zero rows.
    -- Fresh positions isolate the fence from the conflict target (a replay
    -- of the position above would also land zero rows through DO NOTHING).
    UPDATE users SET deleting_at = NOW() WHERE id = 'user-tw-2';
    INSERT INTO tracks (mbid, release_id, title, medium_no, track_no)
    SELECT '33333333-3333-4333-8333-333333333333',
           'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb',
           'Fenced', 1, 1
    FROM (SELECT id FROM users WHERE id = 'user-tw-2' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (release_id, medium_no, track_no) DO NOTHING;
    IF (SELECT count(*) FROM tracks WHERE release_id = 'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb') <> 0 THEN
        RAISE EXCEPTION 'fenced tracks insert landed a row behind the deletion marker';
    END IF;

    -- A missing user fences the same way (no row, so no error either).
    INSERT INTO tracks (mbid, release_id, title, medium_no, track_no)
    SELECT '44444444-4444-4333-8333-444444444444',
           'cccccccc-cccc-cccc-cccc-cccccccccccc',
           'Nobody', 1, 1
    FROM (SELECT id FROM users WHERE id = 'user-tw-gone' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (release_id, medium_no, track_no) DO NOTHING;
    IF (SELECT count(*) FROM tracks WHERE release_id = 'cccccccc-cccc-cccc-cccc-cccccccccccc') <> 0 THEN
        RAISE EXCEPTION 'tracks insert for a missing user produced a row';
    END IF;

    -- Unmatched jobs have no row to write: release_id is NOT NULL, so a
    -- NULL smuggle fails at the schema instead of landing a releaseless
    -- track (the writer skips with reason before ever binding this).
    BEGIN
        INSERT INTO tracks (mbid, release_id, title, medium_no, track_no)
        SELECT '55555555-5555-4333-8333-555555555555',
               NULL,
               'No Release', 1, 1
        FROM (SELECT id FROM users WHERE id = 'user-tw-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence
        ON CONFLICT (release_id, medium_no, track_no) DO NOTHING;
        RAISE EXCEPTION 'tracks accepted a NULL release_id smuggle';
    EXCEPTION WHEN not_null_violation THEN
        NULL;
    END;

    -- Statement 2: track_sources. The envelope version is the 2 literal
    -- (only v2 mint output binds); indexer_id is NULL until the indexers
    -- table lands (the pipeline's indexer_ref is a vault handle, not a
    -- UUID, so there is no id to store).
    INSERT INTO track_sources
        (user_id, track_id, source_job_id, indexer_id, nzb_ref_encrypted,
         nzb_ref_envelope_version, nzb_ref_kek_version,
         nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped,
         nzb_ref_payload_nonce, nzb_ref_payload_format)
    SELECT 'user-tw-1', track_id_var,
           'job_01trackwriters0001', NULL, '\x0304'::bytea,
           2, 1,
           '\x000000000000000000000000'::bytea,
           '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
           '\x000000000000000000000000'::bytea,
           1
    FROM (SELECT id FROM users WHERE id = 'user-tw-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (user_id, track_id) DO NOTHING;
    SELECT count(*) INTO row_count_var FROM track_sources
    WHERE user_id = 'user-tw-1' AND track_id = track_id_var;
    IF row_count_var <> 1 THEN
        RAISE EXCEPTION 'track_sources insert did not land one provenance row';
    END IF;

    -- Idempotent replay converges, and a second job for one (user, track)
    -- slot keeps the first provenance (the slot is single-winner).
    INSERT INTO track_sources
        (user_id, track_id, source_job_id, indexer_id, nzb_ref_encrypted,
         nzb_ref_envelope_version, nzb_ref_kek_version,
         nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped,
         nzb_ref_payload_nonce, nzb_ref_payload_format)
    SELECT 'user-tw-1', track_id_var,
           'job_01trackwriters0001', NULL, '\x0304'::bytea,
           2, 1,
           '\x000000000000000000000000'::bytea,
           '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
           '\x000000000000000000000000'::bytea,
           1
    FROM (SELECT id FROM users WHERE id = 'user-tw-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (user_id, track_id) DO NOTHING;
    INSERT INTO track_sources
        (user_id, track_id, source_job_id, indexer_id, nzb_ref_encrypted,
         nzb_ref_envelope_version, nzb_ref_kek_version,
         nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped,
         nzb_ref_payload_nonce, nzb_ref_payload_format)
    SELECT 'user-tw-1', track_id_var,
           'job_01trackwriters0002', NULL, '\x0506'::bytea,
           2, 1,
           '\x000000000000000000000000'::bytea,
           '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
           '\x000000000000000000000000'::bytea,
           1
    FROM (SELECT id FROM users WHERE id = 'user-tw-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (user_id, track_id) DO NOTHING;
    SELECT source_job_id INTO source_job_var FROM track_sources
    WHERE user_id = 'user-tw-1' AND track_id = track_id_var;
    IF NOT FOUND OR source_job_var <> 'job_01trackwriters0001' THEN
        RAISE EXCEPTION 'track_sources conflict did not keep the first provenance';
    END IF;
    IF (SELECT count(*) FROM track_sources WHERE user_id = 'user-tw-1') <> 1 THEN
        RAISE EXCEPTION 'track_sources replay did not converge on one slot row';
    END IF;

    -- PK re-select (track_writers.rs select_track_source_by_pk_sql shape).
    PERFORM 1 FROM track_sources
    WHERE user_id = 'user-tw-1' AND track_id = track_id_var;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'track_sources PK re-select missed the written row';
    END IF;

    -- Slots are per user: the same track lands a separate row for user-tw-3.
    INSERT INTO track_sources
        (user_id, track_id, source_job_id, indexer_id, nzb_ref_encrypted,
         nzb_ref_envelope_version, nzb_ref_kek_version,
         nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped,
         nzb_ref_payload_nonce, nzb_ref_payload_format)
    SELECT 'user-tw-3', track_id_var,
           'job_01trackwriters0001', NULL, '\x0304'::bytea,
           2, 1,
           '\x000000000000000000000000'::bytea,
           '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
           '\x000000000000000000000000'::bytea,
           1
    FROM (SELECT id FROM users WHERE id = 'user-tw-3' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (user_id, track_id) DO NOTHING;
    IF (SELECT count(*) FROM track_sources WHERE track_id = track_id_var) <> 2 THEN
        RAISE EXCEPTION 'track_sources did not land a separate per-user slot row';
    END IF;

    -- indexer_id binds a UUID once issued (FK-deferred: the indexers table
    -- does not exist yet, so no constraint fires — the column only stores).
    INSERT INTO tracks (mbid, release_id, title, medium_no, track_no)
    SELECT '66666666-6666-4333-8333-666666666666',
           'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa',
           'Second', 1, 2
    FROM (SELECT id FROM users WHERE id = 'user-tw-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (release_id, medium_no, track_no) DO NOTHING;
    INSERT INTO track_sources
        (user_id, track_id, source_job_id, indexer_id, nzb_ref_encrypted,
         nzb_ref_envelope_version, nzb_ref_kek_version,
         nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped,
         nzb_ref_payload_nonce, nzb_ref_payload_format)
    SELECT 'user-tw-1',
           (SELECT id FROM tracks WHERE release_id = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa' AND medium_no = 1 AND track_no = 2),
           'job_01trackwriters0001', 'dddddddd-dddd-dddd-dddd-dddddddddddd', '\x0304'::bytea,
           2, 1,
           '\x000000000000000000000000'::bytea,
           '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
           '\x000000000000000000000000'::bytea,
           1
    FROM (SELECT id FROM users WHERE id = 'user-tw-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (user_id, track_id) DO NOTHING;
    IF (SELECT count(*) FROM track_sources
        WHERE user_id = 'user-tw-1'
          AND indexer_id = 'dddddddd-dddd-dddd-dddd-dddddddddddd') <> 1 THEN
        RAISE EXCEPTION 'track_sources did not store the deferred indexer UUID';
    END IF;

    -- Fenced sources land zero rows without raising (no slot exists yet for
    -- user-tw-2 on this track, so zero rows isolate the fence).
    INSERT INTO track_sources
        (user_id, track_id, source_job_id, indexer_id, nzb_ref_encrypted,
         nzb_ref_envelope_version, nzb_ref_kek_version,
         nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped,
         nzb_ref_payload_nonce, nzb_ref_payload_format)
    SELECT 'user-tw-2', track_id_var,
           'job_01trackwriters0001', NULL, '\x0304'::bytea,
           2, 1,
           '\x000000000000000000000000'::bytea,
           '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
           '\x000000000000000000000000'::bytea,
           1
    FROM (SELECT id FROM users WHERE id = 'user-tw-2' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (user_id, track_id) DO NOTHING;
    IF (SELECT count(*) FROM track_sources WHERE user_id = 'user-tw-2') <> 0 THEN
        RAISE EXCEPTION 'fenced track_sources insert landed a row behind the deletion marker';
    END IF;
    INSERT INTO track_sources
        (user_id, track_id, source_job_id, indexer_id, nzb_ref_encrypted,
         nzb_ref_envelope_version, nzb_ref_kek_version,
         nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped,
         nzb_ref_payload_nonce, nzb_ref_payload_format)
    SELECT 'user-tw-gone', track_id_var,
           'job_01trackwriters0001', NULL, '\x0304'::bytea,
           2, 1,
           '\x000000000000000000000000'::bytea,
           '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
           '\x000000000000000000000000'::bytea,
           1
    FROM (SELECT id FROM users WHERE id = 'user-tw-gone' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (user_id, track_id) DO NOTHING;
    IF (SELECT count(*) FROM track_sources WHERE user_id = 'user-tw-gone') <> 0 THEN
        RAISE EXCEPTION 'track_sources insert for a missing user produced a row';
    END IF;

    -- Sources cannot reference a missing track or job. Both negatives use
    -- fresh (user, track) slots: ON CONFLICT DO NOTHING on a taken slot
    -- would converge instead of reaching the FK.
    BEGIN
        INSERT INTO track_sources
            (user_id, track_id, source_job_id, indexer_id, nzb_ref_encrypted,
             nzb_ref_envelope_version, nzb_ref_kek_version,
             nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped,
             nzb_ref_payload_nonce, nzb_ref_payload_format)
        SELECT 'user-tw-1', 'eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee',
               'job_01trackwriters0001', NULL, '\x0304'::bytea,
               2, 1,
               '\x000000000000000000000000'::bytea,
               '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
               '\x000000000000000000000000'::bytea,
               1
        FROM (SELECT id FROM users WHERE id = 'user-tw-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence
        ON CONFLICT (user_id, track_id) DO NOTHING;
        RAISE EXCEPTION 'FK accepted a source for a missing track';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;
    END;
    -- A third track so the remaining negatives each get a fresh slot.
    INSERT INTO tracks (mbid, release_id, title, medium_no, track_no)
    SELECT '77777777-7777-4333-8333-777777777777',
           'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa',
           'Third', 1, 3
    FROM (SELECT id FROM users WHERE id = 'user-tw-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (release_id, medium_no, track_no) DO NOTHING;
    BEGIN
        INSERT INTO track_sources
            (user_id, track_id, source_job_id, indexer_id, nzb_ref_encrypted,
             nzb_ref_envelope_version, nzb_ref_kek_version,
             nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped,
             nzb_ref_payload_nonce, nzb_ref_payload_format)
        SELECT 'user-tw-3',
               (SELECT id FROM tracks WHERE release_id = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa' AND medium_no = 1 AND track_no = 3),
               'job_missing', NULL, '\x0304'::bytea,
               2, 1,
               '\x000000000000000000000000'::bytea,
               '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
               '\x000000000000000000000000'::bytea,
               1
        FROM (SELECT id FROM users WHERE id = 'user-tw-3' AND deleting_at IS NULL FOR SHARE) AS writer_fence
        ON CONFLICT (user_id, track_id) DO NOTHING;
        RAISE EXCEPTION 'FK accepted a source for a missing job';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;
    END;

    -- Envelope guards: a short nonce is rejected at the write boundary even
    -- when the fence is open (the Rust validator fails these first; the
    -- CHECK backstops callers that skipped it). Fresh slot, so the CHECK —
    -- not the conflict target — decides.
    BEGIN
        INSERT INTO track_sources
            (user_id, track_id, source_job_id, indexer_id, nzb_ref_encrypted,
             nzb_ref_envelope_version, nzb_ref_kek_version,
             nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped,
             nzb_ref_payload_nonce, nzb_ref_payload_format)
        SELECT 'user-tw-1',
               (SELECT id FROM tracks WHERE release_id = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa' AND medium_no = 1 AND track_no = 3),
               'job_01trackwriters0001', NULL, '\x0304'::bytea,
               2, 1,
               '\x00'::bytea,
               '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
               '\x000000000000000000000000'::bytea,
               1
        FROM (SELECT id FROM users WHERE id = 'user-tw-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence
        ON CONFLICT (user_id, track_id) DO NOTHING;
        RAISE EXCEPTION 'CHECK accepted a short nzb_ref wrap nonce';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
END $$;

ROLLBACK;
