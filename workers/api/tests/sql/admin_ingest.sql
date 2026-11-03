-- Regression test for migration 0034 (admin ingest). Run after ALL
-- migrations are applied — twice, since the runner replays every
-- replay-safe migration (see ../run_migration_tests.sh) — so the
-- assertions below also prove the replay did not reset or duplicate
-- anything. Everything happens inside one transaction that is rolled
-- back, so it leaves no rows; any broken expectation RAISEs and (with
-- ON_ERROR_STOP) fails the run.

BEGIN;

DO $$
DECLARE
    job_count INTEGER;
BEGIN
    INSERT INTO users (id, email, created_at)
    VALUES ('user-ai-1', 'ai-1@example.com', 1700000000),
           ('user-ai-2', 'ai-2@example.com', 1700000000);

    -- Pre-existing pipeline rows take the `pipeline` default and keep
    -- their legacy shape (no content columns): the discriminator must
    -- not disturb the pipeline ledger.
    INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
    VALUES ('job_01adminingestpipe', 'user-ai-1', 'ingest:user-ai-1:aa:upload',
            1700000000, 'queued');
    IF (SELECT source FROM ingest_jobs WHERE id = 'job_01adminingestpipe') <> 'pipeline' THEN
        RAISE EXCEPTION 'pipeline row did not default to source pipeline';
    END IF;
    IF (SELECT count(*) FROM ingest_jobs
        WHERE id = 'job_01adminingestpipe' AND content_sha256 IS NULL
          AND nzb_bytes IS NULL AND failure_reason IS NULL) <> 1 THEN
        RAISE EXCEPTION 'pipeline row gained unexpected admin columns';
    END IF;

    -- The source fence: no third source value can be stored.
    BEGIN
        UPDATE ingest_jobs SET source = 'mirror' WHERE id = 'job_01adminingestpipe';
        RAISE EXCEPTION 'CHECK accepted a source outside pipeline/admin';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- The production accepted-upload shape: deterministic content id,
    -- admin source, counts, no failure reason, original row under the
    -- v2 envelope pins (version 2 literal, 12-byte nonces, 48-byte
    -- wrapped DEK) — one transaction (this script's own BEGIN stands
    -- in for it).
    INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status,
                             source, content_sha256, nzb_bytes, file_count, segment_count)
    VALUES ('adm_aiacceptedjob00000000000000000000000000001', 'user-ai-1',
            'admin:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
            1700000000, 'queued', 'admin',
            'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
            100, 1, 2);
    INSERT INTO ingest_originals (job_id, content_sha256, nzb_bytes,
        nzb_ref_encrypted, nzb_ref_envelope_version, nzb_ref_kek_version,
        nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped, nzb_ref_payload_nonce,
        nzb_ref_payload_format, created_at)
    VALUES ('adm_aiacceptedjob00000000000000000000000000001',
            'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
            100, '\x00'::bytea, 2, 1,
            '\x000000000000000000000000'::bytea, '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
            '\x000000000000000000000000'::bytea, 1, 1700000000);
    IF (SELECT count(*) FROM ingest_originals
        WHERE job_id = 'adm_aiacceptedjob00000000000000000000000000001') <> 1 THEN
        RAISE EXCEPTION 'accepted upload did not land an original row';
    END IF;

    -- The failure vocabulary fence: no reason outside the fixed set.
    BEGIN
        INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status,
                                 source, content_sha256, nzb_bytes, failure_reason)
        VALUES ('adm_aibadreason000000000000000000000000000001', 'user-ai-1',
                'admin:aa', 1700000000, 'failed', 'admin',
                'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
                10, 'not xml at all');
        RAISE EXCEPTION 'CHECK accepted a failure reason outside the vocabulary';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- The admin shape coupling: a failed admin row without a reason,
    -- and an accepted admin row without counts, both refuse.
    BEGIN
        INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status,
                                 source, content_sha256, nzb_bytes)
        VALUES ('adm_ainoreason0000000000000000000000000000001', 'user-ai-1',
                'admin:ab', 1700000000, 'failed', 'admin',
                'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
                10);
        RAISE EXCEPTION 'coupling CHECK accepted a failed admin row without a reason';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status,
                                 source, content_sha256, nzb_bytes)
        VALUES ('adm_ainocounts0000000000000000000000000000001', 'user-ai-1',
                'admin:ac', 1700000000, 'queued', 'admin',
                'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
                10);
        RAISE EXCEPTION 'coupling CHECK accepted an accepted admin row without counts';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- The production failed-upload shape: reasoned row, no original.
    INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status,
                             source, content_sha256, nzb_bytes, failure_reason)
    VALUES ('adm_aifailedjob000000000000000000000000000001', 'user-ai-2',
            'admin:ad', 1700000000, 'failed', 'admin',
            'ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb',
            12, 'malformed');
    IF (SELECT count(*) FROM ingest_originals
        WHERE job_id = 'adm_aifailedjob000000000000000000000000000001') <> 0 THEN
        RAISE EXCEPTION 'failed upload landed an original row';
    END IF;

    -- Convergent replay: the deterministic id lets a duplicate upload
    -- ON CONFLICT (id) DO NOTHING to zero further rows.
    INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status,
                             source, content_sha256, nzb_bytes, file_count, segment_count)
    VALUES ('adm_aiacceptedjob00000000000000000000000000001', 'user-ai-2',
            'admin:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
            1700000000, 'queued', 'admin',
            'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
            100, 1, 2)
    ON CONFLICT (id) DO NOTHING;
    SELECT count(*) INTO job_count FROM ingest_jobs
    WHERE content_sha256 = 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855';
    IF job_count <> 1 THEN
        RAISE EXCEPTION 'admin upload replay did not converge on one job row';
    END IF;

    -- The content backstop: a second admin row for one content hash —
    -- even under a different derived id — refuses on the partial
    -- unique index.
    BEGIN
        INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status,
                                 source, content_sha256, nzb_bytes, file_count, segment_count)
        VALUES ('adm_aisecondid0000000000000000000000000000001', 'user-ai-2',
                'admin:ae', 1700000000, 'queued', 'admin',
                'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
                100, 1, 2);
        RAISE EXCEPTION 'partial unique index accepted a second admin row for one hash';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    -- The index is partial: pipeline rows (NULL content) never collide.
    INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
    VALUES ('job_01adminingestpip2', 'user-ai-2', 'ingest:user-ai-2:af:upload',
            1700000000, 'queued');

    -- Envelope pins: a non-v2 version, a short nonce, or a short
    -- wrapped DEK all refuse at the table.
    BEGIN
        INSERT INTO ingest_originals (job_id, content_sha256, nzb_bytes,
            nzb_ref_encrypted, nzb_ref_envelope_version, nzb_ref_kek_version,
            nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped, nzb_ref_payload_nonce,
            nzb_ref_payload_format, created_at)
        VALUES ('adm_aifailedjob000000000000000000000000000001',
                'ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb',
                12, '\x00'::bytea, 1, 1,
                '\x000000000000000000000000'::bytea, '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
                '\x000000000000000000000000'::bytea, 1, 1700000000);
        RAISE EXCEPTION 'envelope CHECK accepted a non-v2 version';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Delete order follows the RESTRICT FKs: the original goes first,
    -- then the job — and a re-upload afterwards lands a fresh row.
    DELETE FROM ingest_originals
    WHERE job_id = 'adm_aiacceptedjob00000000000000000000000000001';
    DELETE FROM ingest_jobs
    WHERE id = 'adm_aiacceptedjob00000000000000000000000000001';
    IF (SELECT count(*) FROM ingest_jobs
        WHERE id = 'adm_aiacceptedjob00000000000000000000000000001') <> 0 THEN
        RAISE EXCEPTION 'delete primitive did not remove the job row';
    END IF;
    INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status,
                             source, content_sha256, nzb_bytes, file_count, segment_count)
    VALUES ('adm_aiacceptedjob00000000000000000000000000001', 'user-ai-2',
            'admin:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
            1700000001, 'queued', 'admin',
            'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
            100, 1, 2)
    ON CONFLICT (id) DO NOTHING;
    IF (SELECT user_id FROM ingest_jobs
        WHERE id = 'adm_aiacceptedjob00000000000000000000000000001') <> 'user-ai-2' THEN
        RAISE EXCEPTION 're-upload after delete did not land a fresh job row';
    END IF;

    -- The extended audit row carries the guarded attempt (actor, job,
    -- outcome, reason) with no subject: writers reference the job by
    -- id in details, as before.
    INSERT INTO audit_log (user_id, action, ok, source_ip, at, subject_user_id, details)
    VALUES ('user-ai-1', 'ingest.uploaded', TRUE, '198.51.100.9', 1700000000, NULL,
            jsonb_build_object('job_id', 'adm_aiacceptedjob00000000000000000000000000001',
                               'outcome', 'accepted'));
    IF (SELECT details ->> 'outcome' FROM audit_log
        WHERE action = 'ingest.uploaded') <> 'accepted'
       OR (SELECT subject_user_id FROM audit_log
        WHERE action = 'ingest.uploaded') IS NOT NULL THEN
        RAISE EXCEPTION 'ingest audit row does not carry job/outcome with null subject';
    END IF;
END $$;

ROLLBACK;
