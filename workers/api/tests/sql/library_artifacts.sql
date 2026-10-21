-- Regression test for migration 0021. Run after
-- ALL migrations are applied (see ../run_migration_tests.sh). Everything
-- happens inside one transaction that is rolled back, so it leaves no rows;
-- any broken expectation RAISEs and (with ON_ERROR_STOP) fails the run.
--
-- This test executes the ingest `opus-64` publication statements from
-- workers/ingest/src/transcode.rs in their exact shape (literals stand in
-- for the $n parameters): the cache_objects upsert, the fenced
-- track_media upsert (format='opus', bitrate=64 literals) and the fenced
-- user_cache_grants upsert, plus the PK re-selects the consumer uses to
-- separate a benign replay from a fenced account.

BEGIN;

DO $$
DECLARE
    media_count INTEGER;
    grant_count INTEGER;
    object_count INTEGER;
BEGIN
    INSERT INTO users (id, email, created_at)
    VALUES ('user-la-1', 'la-1@example.com', 1700000000),
           ('user-la-2', 'la-2@example.com', 1700000000);

    -- Provider row with a fully-populated v2 envelope (rule 1: a
    -- mandatory encrypted field carries all six companions NOT NULL).
    INSERT INTO providers
        (id, user_id, label, encrypted_creds,
         creds_envelope_version, creds_kek_version,
         creds_dek_wrapped_nonce, creds_dek_wrapped,
         creds_payload_nonce, creds_payload_format,
         created_at, last_verified_at)
    VALUES ('11111111-1111-1111-1111-111111111111', 'user-la-1', 'usenet-main',
            '\x0102'::bytea,
            2, 1,
            '\x000000000000000000000000'::bytea,
            '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
            '\x000000000000000000000000'::bytea,
            1,
            1700000000, 1700000000);

    -- A second user with no provider of their own, for the composite-FK
    -- negative below.
    INSERT INTO users (id, email, created_at)
    VALUES ('user-la-3', 'la-3@example.com', 1700000000);

    INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
    VALUES ('job_01testartifact0001', 'user-la-1',
            'ingest:user-la-1:9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734:upload',
            1700000000, 'done');

    INSERT INTO tracks (id, mbid, release_id, title, medium_no, track_no, duration_ms)
    VALUES ('22222222-2222-2222-2222-222222222222',
            '33333333-3333-3333-3333-333333333333',
            '44444444-4444-4444-4444-444444444444',
            'Test Track', 1, 1, 240000);

    -- B6 statement 1: the content-global blob row. No writer fence: the
    -- row carries no user identity. created_at defaults to the DB clock;
    -- ref_count stays NULL (advisory, never the sweep mark set).
    INSERT INTO cache_objects (content_hash, byte_size, r2_bucket, r2_key, last_verified_at)
    VALUES ('9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734',
            1992294, 'nntunev2-audio-staging',
            'audio/9d/70/9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734',
            1700000000)
    ON CONFLICT (content_hash) DO NOTHING;
    SELECT count(*) INTO object_count FROM cache_objects
    WHERE content_hash = '9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734'
      AND created_at IS NOT NULL AND ref_count IS NULL;
    IF object_count <> 1 THEN
        RAISE EXCEPTION 'cache_objects publication did not land with defaulted created_at';
    END IF;

    -- B6 statement 2: the per-user artifact mapping. format/bitrate are
    -- SQL literals pinning the opus-64 profile, not parameters.
    INSERT INTO track_media (user_id, track_id, source_job_id, content_hash, format, bitrate, verified_at)
    SELECT 'user-la-1', '22222222-2222-2222-2222-222222222222',
           'job_01testartifact0001',
           '9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734',
           'opus', 64, 1700000000
    FROM (SELECT id FROM users WHERE id = 'user-la-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (user_id, track_id, source_job_id, content_hash) DO NOTHING;
    SELECT count(*) INTO media_count FROM track_media
    WHERE user_id = 'user-la-1'
      AND track_id = '22222222-2222-2222-2222-222222222222'
      AND format = 'opus' AND bitrate = 64 AND last_served_at IS NOT NULL;
    IF media_count <> 1 THEN
        RAISE EXCEPTION 'track_media publication did not land with initialized last_served_at';
    END IF;

    -- B6 statement 3: the own-provider proof. Without this row the bytes
    -- are unservable.
    INSERT INTO user_cache_grants (user_id, content_hash, provider_id, verified_at)
    SELECT 'user-la-1',
           '9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734',
           '11111111-1111-1111-1111-111111111111', 1700000000
    FROM (SELECT id FROM users WHERE id = 'user-la-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (user_id, content_hash) DO NOTHING;
    SELECT count(*) INTO grant_count FROM user_cache_grants
    WHERE user_id = 'user-la-1' AND last_served_at IS NOT NULL;
    IF grant_count <> 1 THEN
        RAISE EXCEPTION 'user_cache_grants publication did not land with initialized last_served_at';
    END IF;

    -- Idempotent replay: all three statements converge (ON CONFLICT DO
    -- NOTHING), landing zero further rows and no error.
    INSERT INTO cache_objects (content_hash, byte_size, r2_bucket, r2_key, last_verified_at)
    VALUES ('9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734',
            1992294, 'nntunev2-audio-staging',
            'audio/9d/70/9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734',
            1700000000)
    ON CONFLICT (content_hash) DO NOTHING;
    INSERT INTO track_media (user_id, track_id, source_job_id, content_hash, format, bitrate, verified_at)
    SELECT 'user-la-1', '22222222-2222-2222-2222-222222222222',
           'job_01testartifact0001',
           '9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734',
           'opus', 64, 1700000000
    FROM (SELECT id FROM users WHERE id = 'user-la-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (user_id, track_id, source_job_id, content_hash) DO NOTHING;
    INSERT INTO user_cache_grants (user_id, content_hash, provider_id, verified_at)
    SELECT 'user-la-1',
           '9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734',
           '11111111-1111-1111-1111-111111111111', 1700000000
    FROM (SELECT id FROM users WHERE id = 'user-la-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (user_id, content_hash) DO NOTHING;
    IF (SELECT count(*) FROM cache_objects) <> 1
       OR (SELECT count(*) FROM track_media) <> 1
       OR (SELECT count(*) FROM user_cache_grants) <> 1 THEN
        RAISE EXCEPTION 'publication replay did not converge on one row per table';
    END IF;

    -- PK re-selects (transcode.rs select_*_by_pk_sql shapes): the
    -- consumer separates a benign replay from a fenced account by
    -- whether these find the row.
    PERFORM 1 FROM track_media
    WHERE user_id = 'user-la-1'
      AND track_id = '22222222-2222-2222-2222-222222222222'
      AND source_job_id = 'job_01testartifact0001'
      AND content_hash = '9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734';
    IF NOT FOUND THEN
        RAISE EXCEPTION 'track_media PK re-select missed the published row';
    END IF;
    PERFORM 1 FROM user_cache_grants
    WHERE user_id = 'user-la-1'
      AND content_hash = '9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734';
    IF NOT FOUND THEN
        RAISE EXCEPTION 'user_cache_grants PK re-select missed the published row';
    END IF;

    -- Deletion fence (ADR 0002 rule 3): fenced user-row publication for
    -- a deleting account lands zero rows; the unfenced blob row still
    -- lands (content-global, swept when unreferenced, never served).
    UPDATE users SET deleting_at = NOW() WHERE id = 'user-la-2';
    INSERT INTO track_media (user_id, track_id, source_job_id, content_hash, format, bitrate, verified_at)
    SELECT 'user-la-2', '22222222-2222-2222-2222-222222222222',
           'job_01testartifact0001',
           '9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734',
           'opus', 64, 1700000000
    FROM (SELECT id FROM users WHERE id = 'user-la-2' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (user_id, track_id, source_job_id, content_hash) DO NOTHING;
    IF (SELECT count(*) FROM track_media WHERE user_id = 'user-la-2') <> 0 THEN
        RAISE EXCEPTION 'fenced track_media insert landed a row behind the deletion marker';
    END IF;
    INSERT INTO user_cache_grants (user_id, content_hash, provider_id, verified_at)
    SELECT 'user-la-2',
           '9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734',
           '11111111-1111-1111-1111-111111111111', 1700000000
    FROM (SELECT id FROM users WHERE id = 'user-la-2' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (user_id, content_hash) DO NOTHING;
    IF (SELECT count(*) FROM user_cache_grants WHERE user_id = 'user-la-2') <> 0 THEN
        RAISE EXCEPTION 'fenced grant insert landed a row behind the deletion marker';
    END IF;

    -- track_sources: one slot per (user_id, track_id), job-bound.
    INSERT INTO track_sources
        (user_id, track_id, source_job_id, indexer_id, nzb_ref_encrypted,
         nzb_ref_envelope_version, nzb_ref_kek_version,
         nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped,
         nzb_ref_payload_nonce, nzb_ref_payload_format)
    VALUES ('user-la-1', '22222222-2222-2222-2222-222222222222',
            'job_01testartifact0001', NULL, '\x0304'::bytea,
            2, 1,
            '\x000000000000000000000000'::bytea,
            '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
            '\x000000000000000000000000'::bytea,
            1);
    BEGIN
        INSERT INTO track_sources
            (user_id, track_id, source_job_id, indexer_id, nzb_ref_encrypted,
             nzb_ref_envelope_version, nzb_ref_kek_version,
             nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped,
             nzb_ref_payload_nonce, nzb_ref_payload_format)
        VALUES ('user-la-1', '22222222-2222-2222-2222-222222222222',
                'job_01testartifact0001', NULL, '\x0304'::bytea,
                2, 1,
                '\x000000000000000000000000'::bytea,
                '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
                '\x000000000000000000000000'::bytea,
                1);
        RAISE EXCEPTION 'PK accepted a duplicate (user_id, track_id) source';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;

    -- ingest_jobs: idempotent accept on (user_id, idempotency_key).
    BEGIN
        INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
        VALUES ('job_01testartifact0002', 'user-la-1',
                'ingest:user-la-1:9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734:upload',
                1700000000, 'queued');
        RAISE EXCEPTION 'UNIQUE accepted a duplicate (user_id, idempotency_key)';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;

    -- Job ids outside the pipeline alphabet cannot be inserted.
    BEGIN
        INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
        VALUES ('bad job!', 'user-la-1', 'ingest:user-la-1:x:upload', 1700000000, 'queued');
        RAISE EXCEPTION 'CHECK accepted a job id outside the pipeline alphabet';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Unknown job statuses cannot be inserted.
    BEGIN
        INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
        VALUES ('job_01testartifact0003', 'user-la-1', 'ingest:user-la-1:y:upload', 1700000000, 'started');
        RAISE EXCEPTION 'CHECK accepted an unknown ingest status';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Tracks: one row per (release_id, medium_no, track_no) occurrence;
    -- the same recording MBID on two releases is two tracks (MBID not
    -- unique), and medium/track positions are 1-based ordinals.
    INSERT INTO tracks (id, mbid, release_id, title, medium_no, track_no, duration_ms)
    VALUES ('55555555-5555-5555-5555-555555555555',
            '33333333-3333-3333-3333-333333333333',
            '66666666-6666-6666-6666-666666666666',
            'Test Track (other release)', 1, 1, NULL);
    BEGIN
        INSERT INTO tracks (id, release_id, title, medium_no, track_no, duration_ms)
        VALUES ('77777777-7777-7777-7777-777777777777',
                '44444444-4444-4444-4444-444444444444',
                'Duplicate position', 1, 1, NULL);
        RAISE EXCEPTION 'UNIQUE accepted a duplicate (release_id, medium_no, track_no)';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO tracks (id, release_id, title, medium_no, track_no, duration_ms)
        VALUES ('88888888-8888-8888-8888-888888888888',
                '44444444-4444-4444-4444-444444444444',
                'Zero position', 0, 1, NULL);
        RAISE EXCEPTION 'CHECK accepted a non-1-based medium_no';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Content hashes are canonical lowercase hex; anything else is
    -- rejected at the write boundary (R2 keys derive from this form).
    BEGIN
        INSERT INTO cache_objects (content_hash, byte_size, r2_bucket, r2_key, last_verified_at)
        VALUES ('NOT-HEX', 10, 'nntunev2-audio-staging', 'audio/00/00/NOT-HEX', 1700000000);
        RAISE EXCEPTION 'CHECK accepted a non-canonical content hash';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Envelope guards: a wrong envelope version or short nonce is
    -- rejected (rule 1 pins v2 with 12-byte nonces).
    BEGIN
        INSERT INTO providers
            (id, user_id, label, encrypted_creds,
             creds_envelope_version, creds_kek_version,
             creds_dek_wrapped_nonce, creds_dek_wrapped,
             creds_payload_nonce, creds_payload_format,
             created_at, last_verified_at)
        VALUES ('99999999-9999-9999-9999-999999999999', 'user-la-1', 'bad-env',
                '\x0102'::bytea,
                1, 1,
                '\x000000000000000000000000'::bytea,
                '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
                '\x000000000000000000000000'::bytea,
                1,
                1700000000, 1700000000);
        RAISE EXCEPTION 'CHECK accepted a non-v2 envelope version';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO providers
            (id, user_id, label, encrypted_creds,
             creds_envelope_version, creds_kek_version,
             creds_dek_wrapped_nonce, creds_dek_wrapped,
             creds_payload_nonce, creds_payload_format,
             created_at, last_verified_at)
        VALUES ('99999999-9999-9999-9999-999999999999', 'user-la-1', 'bad-nonce',
                '\x0102'::bytea,
                2, 1,
                '\x00'::bytea,
                '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
                '\x000000000000000000000000'::bytea,
                1,
                1700000000, 1700000000);
        RAISE EXCEPTION 'CHECK accepted a short wrap nonce';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Media cannot reference a missing track, job, or object.
    BEGIN
        INSERT INTO track_media (user_id, track_id, source_job_id, content_hash, format, bitrate, verified_at)
        VALUES ('user-la-1', 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa',
                'job_01testartifact0001',
                '9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734',
                'opus', 64, 1700000000);
        RAISE EXCEPTION 'FK accepted media for a missing track';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO track_media (user_id, track_id, source_job_id, content_hash, format, bitrate, verified_at)
        VALUES ('user-la-1', '22222222-2222-2222-2222-222222222222',
                'job_missing',
                '9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734',
                'opus', 64, 1700000000);
        RAISE EXCEPTION 'FK accepted media for a missing job';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;
    END;

    -- Grants cannot reference a missing object or provider, and the
    -- composite FK binds the provider to the same user: user-la-3
    -- cannot present user-la-1's provider as their own proof.
    BEGIN
        INSERT INTO user_cache_grants (user_id, content_hash, provider_id, verified_at)
        VALUES ('user-la-1',
                'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                '11111111-1111-1111-1111-111111111111', 1700000000);
        RAISE EXCEPTION 'FK accepted a grant for a missing object';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO user_cache_grants (user_id, content_hash, provider_id, verified_at)
        VALUES ('user-la-3',
                '9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734',
                '11111111-1111-1111-1111-111111111111', 1700000000);
        RAISE EXCEPTION 'composite FK accepted another user''s provider as grant proof';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;
    END;

    -- Bitrates are positive.
    BEGIN
        INSERT INTO track_media (user_id, track_id, source_job_id, content_hash, format, bitrate, verified_at)
        VALUES ('user-la-1', '22222222-2222-2222-2222-222222222222',
                'job_01testartifact0001',
                '9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734',
                'opus', 0, 1700000000);
        RAISE EXCEPTION 'CHECK accepted a non-positive bitrate';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- RESTRICT (ADR 0002 rule 3): deletion is an explicit batched
    -- procedure — a bare parent delete fails instead of cascading.
    BEGIN
        DELETE FROM users WHERE id = 'user-la-1';
        RAISE EXCEPTION 'user delete cascaded past RESTRICT children';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;
    END;
    BEGIN
        DELETE FROM ingest_jobs WHERE id = 'job_01testartifact0001';
        RAISE EXCEPTION 'job delete cascaded past RESTRICT media';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;
    END;
    BEGIN
        DELETE FROM cache_objects
        WHERE content_hash = '9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734';
        RAISE EXCEPTION 'object delete cascaded past RESTRICT grants';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;
    END;

    -- Sweep step-5 mark indexes plus the GC/RI paths (ADR 0002 rule 3,
    -- ADR 0003 §collection step 5) all exist.
    IF (SELECT count(*) FROM pg_indexes WHERE indexname IN
        ('idx_track_media_hash', 'idx_user_cache_grants_hash',
         'idx_track_media_track', 'idx_track_media_job',
         'idx_track_sources_track', 'idx_track_sources_job',
         'idx_tracks_mbid')) <> 7 THEN
        RAISE EXCEPTION 'expected all seven 0021 access-path indexes to exist';
    END IF;
END $$;

ROLLBACK;
