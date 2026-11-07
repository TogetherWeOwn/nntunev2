-- Posting-disabled preparation checkpoint and outbox in the existing job DB.
-- One immutable input/request per explicit job-attempt predecessor, not a second
-- generation registry. References are opaque storage identities, never filenames,
-- article lists or plaintext manifests. Object readers must resolve immutable
-- versions and check the stored byte digests before delivery. No legacy backfill,
-- worker activation, cutoff change or cleanup authority is introduced.

SET search_path = public, pg_catalog;

CREATE TABLE IF NOT EXISTS ingest_file_dispatches (
    id TEXT PRIMARY KEY DEFAULT gen_random_uuid()::text,
    job_id TEXT NOT NULL REFERENCES ingest_jobs(id) ON DELETE RESTRICT,
    operation_key TEXT NOT NULL CHECK (operation_key ~ '^[A-Za-z0-9_-]{1,64}$'),
    purpose TEXT NOT NULL DEFAULT 'prepare' CHECK (purpose = 'prepare'),
    owner_id TEXT NOT NULL,
    admission_key TEXT NOT NULL,
    original_sha256 TEXT NOT NULL CHECK (original_sha256 ~ '^[0-9a-f]{64}$'),
    file_index INTEGER NOT NULL CHECK (file_index >= 0),
    expected_attempt BIGINT NOT NULL CHECK (expected_attempt >= 0 AND expected_attempt < 9223372036854775807),
    object_id TEXT NOT NULL CHECK (object_id ~ '^[A-Za-z0-9_-]{1,64}$'),
    object_version TEXT NOT NULL CHECK (object_version ~ '^[A-Za-z0-9_-]{1,64}$'),
    plaintext_bytes BIGINT NOT NULL CHECK (plaintext_bytes > 0),
    plaintext_sha256 TEXT NOT NULL CHECK (plaintext_sha256 ~ '^[0-9a-f]{64}$'),
    plan_id TEXT NOT NULL CHECK (plan_id ~ '^[A-Za-z0-9_-]{1,64}$'),
    plan_version TEXT NOT NULL CHECK (plan_version ~ '^[A-Za-z0-9_-]{1,64}$'),
    plan_format SMALLINT NOT NULL CHECK (plan_format = 1),
    plan_bytes BIGINT NOT NULL CHECK (plan_bytes > 0),
    plan_sha256 TEXT NOT NULL CHECK (plan_sha256 ~ '^[0-9a-f]{64}$'),
    send_acknowledged BOOLEAN NOT NULL DEFAULT FALSE,
    UNIQUE (job_id, operation_key),
    UNIQUE (job_id, expected_attempt)
);

CREATE OR REPLACE FUNCTION protect_ingest_file_dispatch() RETURNS trigger
LANGUAGE plpgsql SET search_path = public, pg_catalog AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION USING ERRCODE = '23514', MESSAGE = 'file dispatch is immutable';
    END IF;
    IF (to_jsonb(NEW) - 'send_acknowledged') IS DISTINCT FROM
       (to_jsonb(OLD) - 'send_acknowledged') OR
       (OLD.send_acknowledged AND NOT NEW.send_acknowledged) THEN
        RAISE EXCEPTION USING ERRCODE = '23514', MESSAGE = 'file dispatch is immutable';
    END IF;
    RETURN NEW;
END $$;

DROP TRIGGER IF EXISTS ingest_file_dispatch_immutable ON ingest_file_dispatches;
CREATE TRIGGER ingest_file_dispatch_immutable
    BEFORE UPDATE OR DELETE ON ingest_file_dispatches
    FOR EACH ROW EXECUTE FUNCTION protect_ingest_file_dispatch();

CREATE INDEX IF NOT EXISTS ingest_file_dispatch_pending_idx
    ON ingest_file_dispatches(job_id, expected_attempt) WHERE NOT send_acknowledged;
