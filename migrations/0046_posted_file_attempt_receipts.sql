-- Durable pre-commit attempt receipts for a trusted job coordinator. A stable
-- request identity is scoped to a job and its authenticated owner; it is NOT a
-- Queue/KV attempt hint or permission to read the current attempt token.
-- Preparation grants no lease or posting authority. Acquisition/recovery must
-- still prove the receipt's exact live token against posted_file_job_attempts.
-- Receipts are immutable, including after expiry/supersession, so replay cannot
-- renew a lease, remint a token or silently target a newer attempt. No existing
-- attempt, generation, custody, health, retirement gate or envelope is changed.
-- No legacy receipt is manufactured and no receipt cleanup is introduced.

SET search_path = public, pg_catalog;

CREATE TABLE IF NOT EXISTS posted_file_attempt_receipts (
    job_id TEXT NOT NULL REFERENCES ingest_jobs(id) ON DELETE RESTRICT,
    request_id TEXT NOT NULL CHECK (request_id ~ '^[A-Za-z0-9_-]{1,64}$'),
    owner_id TEXT NOT NULL CHECK (owner_id ~ '^[A-Za-z0-9_-]{1,64}$'),
    attempt BIGINT NOT NULL CHECK (attempt >= 1),
    token TEXT NOT NULL UNIQUE CHECK (token ~ '^[A-Za-z0-9_-]{1,64}$'),
    PRIMARY KEY (job_id, request_id)
);

CREATE OR REPLACE FUNCTION protect_posted_file_attempt_receipt() RETURNS trigger
LANGUAGE plpgsql SET search_path = public, pg_catalog AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION USING ERRCODE = '23514',
            MESSAGE = 'posted attempt receipt is immutable';
    END IF;
    IF NEW IS DISTINCT FROM OLD THEN
        RAISE EXCEPTION USING ERRCODE = '23514',
            MESSAGE = 'posted attempt receipt is immutable';
    END IF;
    RETURN NEW;
END $$;

DROP TRIGGER IF EXISTS posted_file_attempt_receipt_immutable ON posted_file_attempt_receipts;
CREATE TRIGGER posted_file_attempt_receipt_immutable
    BEFORE UPDATE OR DELETE ON posted_file_attempt_receipts
    FOR EACH ROW EXECUTE FUNCTION protect_posted_file_attempt_receipt();
