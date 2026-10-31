-- Regression test for the ingest_jobs writer
-- (workers/ingest/src/ingest_jobs.rs against migration 0022). Run after
-- ALL migrations are applied (see ../run_migration_tests.sh). Everything
-- happens inside one transaction that is rolled back, so it leaves no rows;
-- any broken expectation RAISEs and (with ON_ERROR_STOP) fails the run.
--
-- This test executes the admission upsert, the terminal transition, and
-- both re-selects in their exact shapes (literals stand in for the $n
-- parameters): the fenced queued insert with ON CONFLICT (id) DO NOTHING,
-- the fenced queued/running-to-done/failed update, the live-owner identity
-- re-select the admission path uses to separate a converged duplicate from
-- a deleting/missing-owner replay, and the status re-select the consumer
-- uses to separate a converged replay from a missing-or-fenced row.

BEGIN;

DO $$
DECLARE
    updated_count INTEGER;
    row_count_var INTEGER;
    job_status TEXT;
    job_owner TEXT;
    job_key TEXT;
BEGIN
    INSERT INTO users (id, email, created_at)
    VALUES ('user-ij-1', 'ij-1@example.com', 1700000000),
           ('user-ij-2', 'ij-2@example.com', 1700000000),
           ('user-ij-3', 'ij-3@example.com', 1700000000);

    -- Statement 1: admission. The status is the 'queued' literal, never
    -- a caller value; the conflict target is the PK.
    INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
    SELECT 'job_01ingestjobs0001', 'user-ij-1',
           'ingest:user-ij-1:9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734:upload',
           1700000000, 'queued'
    FROM (SELECT id FROM users WHERE id = 'user-ij-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (id) DO NOTHING;
    SELECT count(*) INTO row_count_var FROM ingest_jobs
    WHERE id = 'job_01ingestjobs0001' AND status = 'queued';
    IF row_count_var <> 1 THEN
        RAISE EXCEPTION 'ingest_jobs admission did not land one queued row';
    END IF;

    -- Idempotent replay converges on the same id (zero further rows).
    INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
    SELECT 'job_01ingestjobs0001', 'user-ij-1',
           'ingest:user-ij-1:9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734:upload',
           1700000000, 'queued'
    FROM (SELECT id FROM users WHERE id = 'user-ij-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (id) DO NOTHING;
    IF (SELECT count(*) FROM ingest_jobs WHERE user_id = 'user-ij-1') <> 1 THEN
        RAISE EXCEPTION 'ingest_jobs admission replay did not converge';
    END IF;

    -- A mismatched id for one (user, key) pair is a pipeline bug: the PK
    -- conflict target lets the UNIQUE raise 23505 instead of converging
    -- the pipeline onto a row for a different job.
    BEGIN
        INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
        SELECT 'job_01ingestjobs9999', 'user-ij-1',
               'ingest:user-ij-1:9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734:upload',
               1700000000, 'queued'
        FROM (SELECT id FROM users WHERE id = 'user-ij-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence
        ON CONFLICT (id) DO NOTHING;
        RAISE EXCEPTION 'admission swallowed a mismatched id for one key';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;

    -- Statement 2: terminal transition queued -> done matches one row.
    UPDATE ingest_jobs SET status = 'done'
    FROM (SELECT id FROM users WHERE id = 'user-ij-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    WHERE ingest_jobs.id = 'job_01ingestjobs0001' AND ingest_jobs.user_id = 'user-ij-1' AND ingest_jobs.user_id = writer_fence.id AND ingest_jobs.status IN ('queued', 'running');
    GET DIAGNOSTICS updated_count = ROW_COUNT;
    IF updated_count <> 1 THEN
        RAISE EXCEPTION 'terminal transition did not match the queued row';
    END IF;

    -- A converged replay matches zero rows, and the status re-select
    -- finds the terminal status (classify: Converged).
    UPDATE ingest_jobs SET status = 'done'
    FROM (SELECT id FROM users WHERE id = 'user-ij-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    WHERE ingest_jobs.id = 'job_01ingestjobs0001' AND ingest_jobs.user_id = 'user-ij-1' AND ingest_jobs.user_id = writer_fence.id AND ingest_jobs.status IN ('queued', 'running');
    GET DIAGNOSTICS updated_count = ROW_COUNT;
    IF updated_count <> 0 THEN
        RAISE EXCEPTION 'terminal transition rewrote an already-terminal row';
    END IF;
    SELECT status INTO job_status FROM ingest_jobs
    WHERE id = 'job_01ingestjobs0001' AND user_id = 'user-ij-1';
    IF NOT FOUND OR job_status <> 'done' THEN
        RAISE EXCEPTION 'ingest_jobs status re-select missed the settled row';
    END IF;

    -- running -> failed also moves forward (the reserved lease state
    -- terminates the same way when it exists).
    INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
    VALUES ('job_01ingestjobs0002', 'user-ij-1',
            'ingest:user-ij-1:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:upload',
            1700000000, 'running');
    UPDATE ingest_jobs SET status = 'failed'
    FROM (SELECT id FROM users WHERE id = 'user-ij-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    WHERE ingest_jobs.id = 'job_01ingestjobs0002' AND ingest_jobs.user_id = 'user-ij-1' AND ingest_jobs.user_id = writer_fence.id AND ingest_jobs.status IN ('queued', 'running');
    GET DIAGNOSTICS updated_count = ROW_COUNT;
    IF updated_count <> 1 THEN
        RAISE EXCEPTION 'terminal transition did not match the running row';
    END IF;

    -- Unknown statuses are rejected by the CHECK backstop even if a
    -- caller skipped validate_transition_status. The target row must be
    -- pre-terminal: the predicate skips terminal rows, so testing against
    -- a settled row would match nothing and the sentinel RAISE below
    -- would fire instead of the CHECK.
    INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
    VALUES ('job_01ingestjobs0006', 'user-ij-1',
            'ingest:user-ij-1:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee:upload',
            1700000000, 'queued');
    BEGIN
        UPDATE ingest_jobs SET status = 'started'
        FROM (SELECT id FROM users WHERE id = 'user-ij-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence
        WHERE ingest_jobs.id = 'job_01ingestjobs0006' AND ingest_jobs.user_id = 'user-ij-1' AND ingest_jobs.user_id = writer_fence.id AND ingest_jobs.status IN ('queued', 'running');
        RAISE EXCEPTION 'CHECK accepted an unknown ingest status on transition';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Deletion fence (ADR 0002 rule 3): admission for a deleting account
    -- lands zero rows without raising, and the terminal transition on
    -- their still-queued row matches zero rows while the re-select still
    -- finds it queued (classify: MissingOrFenced).
    UPDATE users SET deleting_at = NOW() WHERE id = 'user-ij-2';
    INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
    SELECT 'job_01ingestjobs0003', 'user-ij-2',
           'ingest:user-ij-2:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb:upload',
           1700000000, 'queued'
    FROM (SELECT id FROM users WHERE id = 'user-ij-2' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (id) DO NOTHING;
    IF (SELECT count(*) FROM ingest_jobs WHERE id = 'job_01ingestjobs0003') <> 0 THEN
        RAISE EXCEPTION 'fenced admission landed a row behind the deletion marker';
    END IF;
    INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
    VALUES ('job_01ingestjobs0004', 'user-ij-2',
            'ingest:user-ij-2:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc:upload',
            1700000000, 'queued');
    UPDATE ingest_jobs SET status = 'done'
    FROM (SELECT id FROM users WHERE id = 'user-ij-2' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    WHERE ingest_jobs.id = 'job_01ingestjobs0004' AND ingest_jobs.user_id = 'user-ij-2' AND ingest_jobs.user_id = writer_fence.id AND ingest_jobs.status IN ('queued', 'running');
    GET DIAGNOSTICS updated_count = ROW_COUNT;
    IF updated_count <> 0 THEN
        RAISE EXCEPTION 'fenced transition matched a row behind the deletion marker';
    END IF;
    SELECT status INTO job_status FROM ingest_jobs
    WHERE id = 'job_01ingestjobs0004' AND user_id = 'user-ij-2';
    IF NOT FOUND OR job_status <> 'queued' THEN
        RAISE EXCEPTION 'fenced row did not stay queued for classification';
    END IF;

    -- A job id presented under the wrong user matches nothing: the row
    -- match repeats the fence owner ($2), so cross-user transitions fail
    -- closed instead of advancing another user's job. user-ij-3 is active
    -- (fence open) and job_01ingestjobs0004 is still queued (status
    -- predicate passes), so the zero rows isolate the owner binding.
    UPDATE ingest_jobs SET status = 'failed'
    FROM (SELECT id FROM users WHERE id = 'user-ij-3' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    WHERE ingest_jobs.id = 'job_01ingestjobs0004' AND ingest_jobs.user_id = 'user-ij-3' AND ingest_jobs.user_id = writer_fence.id AND ingest_jobs.status IN ('queued', 'running');
    GET DIAGNOSTICS updated_count = ROW_COUNT;
    IF updated_count <> 0 THEN
        RAISE EXCEPTION 'cross-user transition matched another user''s job';
    END IF;

    -- Admission for a missing user produces zero rows through the fence
    -- (no row, so no FK violation either): the fence decides, not the
    -- constraint.
    INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
    SELECT 'job_01ingestjobs0005', 'user-ij-gone',
           'ingest:user-ij-gone:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd:upload',
           1700000000, 'queued'
    FROM (SELECT id FROM users WHERE id = 'user-ij-gone' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (id) DO NOTHING;
    IF (SELECT count(*) FROM ingest_jobs WHERE id = 'job_01ingestjobs0005') <> 0 THEN
        RAISE EXCEPTION 'admission for a missing user produced a row';
    END IF;

    -- Statement 3: admission identity re-select. A live duplicate replay
    -- returns the stored owner + key through the live-owner join, while a
    -- deleting owner or an unknown job id returns zero rows, so the caller
    -- separates a converged duplicate from a fenced replay without adopting
    -- another owner's job. The join + deleting_at predicate + FOR SHARE OF
    -- users are all load-bearing: the lock pins the live owner row while
    -- the caller compares identity. A true missing-owner row (job present,
    -- owner absent) cannot persist under the RESTRICT FK (proven below),
    -- so the deleting-owner + unknown-id cases cover every representable
    -- replay; the orphan SELECT shape is unrepresentable by construction.
    SELECT ingest_jobs.user_id, ingest_jobs.idempotency_key INTO job_owner, job_key
    FROM ingest_jobs JOIN users ON users.id = ingest_jobs.user_id
    WHERE ingest_jobs.id = 'job_01ingestjobs0001' AND users.deleting_at IS NULL FOR SHARE OF users;
    IF NOT FOUND OR job_owner <> 'user-ij-1' OR job_key <> 'ingest:user-ij-1:9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734:upload' THEN
        RAISE EXCEPTION 'identity re-select missed the live duplicate';
    END IF;

    -- Deleting-owner replay: job_01ingestjobs0004 exists and stays queued,
    -- but its owner carries the deletion marker, so the live-owner join
    -- returns zero rows instead of the stored identity.
    SELECT ingest_jobs.user_id, ingest_jobs.idempotency_key INTO job_owner, job_key
    FROM ingest_jobs JOIN users ON users.id = ingest_jobs.user_id
    WHERE ingest_jobs.id = 'job_01ingestjobs0004' AND users.deleting_at IS NULL FOR SHARE OF users;
    IF FOUND THEN
        RAISE EXCEPTION 'identity re-select adopted a deleting owner';
    END IF;

    -- Missing-job replay (unknown id): job_01ingestjobs0005 never landed
    -- (fenced admission above), so the same statement returns zero rows
    -- for the unknown id rather than a stale identity. This is distinct
    -- from a missing-owner row, which the FK guard below proves cannot
    -- persist.
    SELECT ingest_jobs.user_id, ingest_jobs.idempotency_key INTO job_owner, job_key
    FROM ingest_jobs JOIN users ON users.id = ingest_jobs.user_id
    WHERE ingest_jobs.id = 'job_01ingestjobs0005' AND users.deleting_at IS NULL FOR SHARE OF users;
    IF FOUND THEN
        RAISE EXCEPTION 'identity re-select found a missing job';
    END IF;

    -- True missing-owner gap: a job row whose owner row is gone would also
    -- return zero rows through the inner join, but the RESTRICT FK rejects
    -- the orphan insert, and the fence rejects admission without a live
    -- owner, so no persistent orphan exists to re-select. The FK is not
    -- deferrable (migration 0022 declares no DEFERRABLE), so even a
    -- deferred-constraint probe cannot stage one; constraints stay enabled.
    -- Prove the guard still holds.
    BEGIN
        INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
        VALUES ('job_01ingestjobs0007', 'user-ij-gone',
                'ingest:user-ij-gone:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd:upload',
                1700000000, 'queued');
        RAISE EXCEPTION 'orphan job row escaped the owner FK';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;
    END;
END $$;

ROLLBACK;
