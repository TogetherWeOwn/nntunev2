-- API-owned encrypted preparation plans; never plaintext in DB, R2 or Queue.
-- Envelope-v2 AAD: domain=nntune-envelope-v2, purpose=payload/dek,
-- table=ingest_preparation_plans, field=plan, owner_scope=owner_id,
-- row_key=id (framed job/operation/file digest), envelope_version=2,
-- payload_format=1. KEK version is included only for purpose=dek.
-- Identity and envelopes are frozen together. No legacy backfill or activation.
-- Prior KEKs must be retained: wrap-only rotation/retirement needs its own
-- reviewed adapter before any key can be removed. This adds no key store.

SET search_path = public, pg_catalog;

CREATE TABLE IF NOT EXISTS ingest_preparation_plans (
    id TEXT PRIMARY KEY CHECK (id ~ '^[0-9a-f]{64}$'),
    version TEXT NOT NULL CHECK (version ~ '^[0-9a-f]{32}$'),
    job_id TEXT NOT NULL REFERENCES ingest_jobs(id) ON DELETE RESTRICT,
    operation_key TEXT NOT NULL CHECK (operation_key ~ '^[A-Za-z0-9_-]{1,64}$'),
    file_index INTEGER NOT NULL CHECK (file_index >= 0),
    owner_id TEXT NOT NULL,
    admission_key TEXT NOT NULL,
    original_sha256 TEXT NOT NULL CHECK (original_sha256 ~ '^[0-9a-f]{64}$'),
    original_bytes BIGINT NOT NULL CHECK (original_bytes > 0),
    file_count INTEGER NOT NULL CHECK (file_count > 0 AND file_index < file_count),
    plaintext_bytes INTEGER NOT NULL CHECK (plaintext_bytes BETWEEN 1 AND 1048576),
    plaintext_sha256 TEXT NOT NULL CHECK (plaintext_sha256 ~ '^[0-9a-f]{64}$'),
    envelope_version SMALLINT NOT NULL CHECK (envelope_version = 2),
    payload_format SMALLINT NOT NULL CHECK (payload_format = 1),
    kek_version INTEGER NOT NULL CHECK (kek_version >= 0),
    dek_wrapped_nonce BYTEA NOT NULL CHECK (octet_length(dek_wrapped_nonce) = 12),
    dek_wrapped BYTEA NOT NULL CHECK (octet_length(dek_wrapped) = 48),
    payload_nonce BYTEA NOT NULL CHECK (octet_length(payload_nonce) = 12),
    encrypted BYTEA NOT NULL CHECK (octet_length(encrypted) = plaintext_bytes + 16),
    UNIQUE (job_id, operation_key)
);

CREATE OR REPLACE FUNCTION protect_ingest_preparation_plan() RETURNS trigger
LANGUAGE plpgsql SET search_path = public, pg_catalog AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION USING ERRCODE = '23514', MESSAGE = 'preparation plan is immutable';
    END IF;
    IF NEW IS DISTINCT FROM OLD THEN
        RAISE EXCEPTION USING ERRCODE = '23514', MESSAGE = 'preparation plan is immutable';
    END IF;
    RETURN NEW;
END $$;

DROP TRIGGER IF EXISTS ingest_preparation_plan_immutable ON ingest_preparation_plans;
CREATE TRIGGER ingest_preparation_plan_immutable
    BEFORE UPDATE OR DELETE ON ingest_preparation_plans
    FOR EACH ROW EXECUTE FUNCTION protect_ingest_preparation_plan();
