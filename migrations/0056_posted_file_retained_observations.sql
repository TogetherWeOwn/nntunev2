-- Original-read provenance for retained negative health observations. These
-- records are NOT conclusive retention failure, topology, replacement or cleanup
-- authority. No historical observation is backfilled or upgraded into proof.
-- References and attempts may advance; preserve their original incarnation here
-- rather than attaching historical evidence to their mutable current rows.
SET search_path = public, pg_catalog;

CREATE TABLE IF NOT EXISTS posted_file_retained_observations (
    generation_id TEXT NOT NULL,
    plaintext_sha256 TEXT NOT NULL,
    health_revision BIGINT NOT NULL CHECK (health_revision >= 0),
    round BIGINT NOT NULL CHECK (round >= 0),
    round_token TEXT NOT NULL CHECK (round_token ~ '^[A-Za-z0-9_-]{1,64}$'),
    job_id TEXT NOT NULL REFERENCES ingest_jobs(id) ON DELETE RESTRICT,
    attempt BIGINT NOT NULL CHECK (attempt >= 1),
    attempt_token TEXT NOT NULL CHECK (attempt_token ~ '^[A-Za-z0-9_-]{1,64}$'),
    reference_kind TEXT NOT NULL CHECK (reference_kind IN ('legacy_pin', 'serving', 'rollback')),
    reference_owner TEXT NOT NULL CHECK (reference_owner ~ '^[A-Za-z0-9_-]{1,64}$'),
    reference_token TEXT NOT NULL CHECK (reference_token ~ '^[A-Za-z0-9_-]{1,64}$'),
    prepared_registry_revision BIGINT NOT NULL CHECK (prepared_registry_revision >= 0),
    prepared_selection TEXT,
    recorded_registry_revision BIGINT NOT NULL CHECK (recorded_registry_revision >= 0),
    recorded_selection TEXT,
    side SMALLINT CHECK (side IN (0, 1)),
    state TEXT NOT NULL CHECK (state IN ('unknown', 'unavailable', 'authentication_failed', 'integrity_failed')),
    recorded_at BIGINT NOT NULL DEFAULT (extract(epoch FROM clock_timestamp())::BIGINT)
        CHECK (recorded_at BETWEEN 0 AND 253402300799),
    PRIMARY KEY (generation_id, health_revision),
    FOREIGN KEY (generation_id, plaintext_sha256)
        REFERENCES posted_files(id, plaintext_sha256) ON DELETE RESTRICT,
    FOREIGN KEY (prepared_selection, plaintext_sha256)
        REFERENCES posted_files(id, plaintext_sha256) ON DELETE RESTRICT,
    FOREIGN KEY (recorded_selection, plaintext_sha256)
        REFERENCES posted_files(id, plaintext_sha256) ON DELETE RESTRICT,
    CHECK ((state = 'unknown') = (side IS NULL))
);

CREATE OR REPLACE FUNCTION protect_posted_file_retained_observation() RETURNS trigger
LANGUAGE plpgsql SET search_path = public, pg_catalog AS $$
BEGIN
    IF TG_OP = 'DELETE' OR NEW IS DISTINCT FROM OLD THEN
        RAISE EXCEPTION USING ERRCODE = '23514',
            MESSAGE = 'retained observation provenance is immutable';
    END IF;
    RETURN NEW;
END $$;
DROP TRIGGER IF EXISTS posted_file_retained_observation_immutable
    ON posted_file_retained_observations;
CREATE TRIGGER posted_file_retained_observation_immutable
    BEFORE UPDATE OR DELETE ON posted_file_retained_observations
    FOR EACH ROW EXECUTE FUNCTION protect_posted_file_retained_observation();
