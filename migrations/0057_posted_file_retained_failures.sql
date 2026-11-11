-- Repair-first retained-policy decisions. A negative observation alone never
-- becomes a failure. The adapter snapshots the complete current repair evidence
-- atomically with terminal retention health under the selected-generation fence.
-- This is not physical-loss, multi-generation admission, rollout or GC authority.
-- No existing health/observation is backfilled, and uniqueness/cutoff stay intact.
SET search_path = public, pg_catalog;

CREATE TABLE IF NOT EXISTS posted_file_retained_failures (
    generation_id TEXT NOT NULL REFERENCES posted_files(id) ON DELETE RESTRICT,
    health_revision BIGINT NOT NULL CHECK (health_revision >= 0),
    round BIGINT NOT NULL CHECK (round >= 1),
    round_token TEXT NOT NULL CHECK (round_token ~ '^[A-Za-z0-9_-]{1,64}$'),
    origin_health_revision BIGINT NOT NULL CHECK (origin_health_revision < health_revision),
    job_id TEXT NOT NULL REFERENCES ingest_jobs(id) ON DELETE RESTRICT,
    attempt BIGINT NOT NULL CHECK (attempt >= 1),
    attempt_token TEXT NOT NULL CHECK (attempt_token ~ '^[A-Za-z0-9_-]{1,64}$'),
    reference_kind TEXT NOT NULL CHECK (reference_kind IN ('legacy_pin', 'serving', 'rollback')),
    reference_owner TEXT NOT NULL CHECK (reference_owner ~ '^[A-Za-z0-9_-]{1,64}$'),
    reference_token TEXT NOT NULL CHECK (reference_token ~ '^[A-Za-z0-9_-]{1,64}$'),
    registry_revision BIGINT NOT NULL CHECK (registry_revision >= 0),
    outcome_revision BIGINT NOT NULL CHECK (outcome_revision = health_revision + 1),
    outcome_round BIGINT NOT NULL CHECK (outcome_round = round + 1),
    outcome_token TEXT NOT NULL CHECK (outcome_token ~ '^[A-Za-z0-9_-]{1,64}$'),
    repair_evidence JSONB NOT NULL CHECK (
        jsonb_typeof(repair_evidence) = 'object'
        AND repair_evidence ?& ARRAY['schedule', 'slots', 'content']
        AND jsonb_typeof(repair_evidence->'schedule') = 'object'
        AND jsonb_typeof(repair_evidence->'slots') = 'array'
        AND jsonb_typeof(repair_evidence->'content') = 'array'
    ),
    decided_at BIGINT NOT NULL DEFAULT (extract(epoch FROM clock_timestamp())::BIGINT)
        CHECK (decided_at BETWEEN 0 AND 253402300799),
    PRIMARY KEY (generation_id, health_revision),
    FOREIGN KEY (generation_id, origin_health_revision)
        REFERENCES posted_file_retained_observations(generation_id, health_revision) ON DELETE RESTRICT
);

CREATE OR REPLACE FUNCTION protect_posted_file_retained_failure() RETURNS trigger
LANGUAGE plpgsql SET search_path = public, pg_catalog AS $$
BEGIN
    IF TG_OP = 'DELETE' OR NEW IS DISTINCT FROM OLD THEN
        RAISE EXCEPTION USING ERRCODE = '23514',
            MESSAGE = 'retained failure decision is immutable';
    END IF;
    RETURN NEW;
END $$;
DROP TRIGGER IF EXISTS posted_file_retained_failure_immutable ON posted_file_retained_failures;
CREATE TRIGGER posted_file_retained_failure_immutable
    BEFORE UPDATE OR DELETE ON posted_file_retained_failures
    FOR EACH ROW EXECUTE FUNCTION protect_posted_file_retained_failure();
