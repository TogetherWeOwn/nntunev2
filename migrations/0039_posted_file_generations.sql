-- Immutable posted-file generations. The existing posted_files row remains
-- the custody object and its ID remains the serving/rollback identity. Envelope
-- v2 AAD remains hash-bound to posted_files; no ciphertext or AAD is rewritten.
--
-- This preparation migration deliberately RETAINS the unique plaintext hash
-- constraint. Multi-generation admission is not enabled until the legacy
-- writers, health callbacks and publication consumers are fenced in the same
-- rollout. The adapter rejects a replacement while this constraint remains.
-- No legacy verification flag or timestamp is promoted into new health proof.
-- All pre-existing custody is pinned conservatively, without enumerating or
-- deleting unknown references. No garbage collector is introduced.

SET search_path = public, pg_catalog;

ALTER TABLE posted_files ADD COLUMN IF NOT EXISTS generation_operation TEXT
    CHECK (generation_operation IS NULL OR generation_operation ~ '^[A-Za-z0-9_-]{1,64}$');
ALTER TABLE posted_files ADD COLUMN IF NOT EXISTS plan_sha256 TEXT
    CHECK (plan_sha256 IS NULL OR plan_sha256 ~ '^[0-9a-f]{64}$');

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                   WHERE conrelid = 'public.posted_files'::regclass
                     AND conname = 'posted_files_generation_identity') THEN
        ALTER TABLE posted_files ADD CONSTRAINT posted_files_generation_identity
            UNIQUE (id, plaintext_sha256);
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                   WHERE conrelid = 'public.posted_files'::regclass
                     AND conname = 'posted_files_generation_bundle') THEN
        ALTER TABLE posted_files ADD CONSTRAINT posted_files_generation_bundle CHECK (
            (generation_operation IS NULL AND plan_sha256 IS NULL)
            OR (generation_operation IS NOT NULL AND plan_sha256 IS NOT NULL
                AND plaintext_size IS NOT NULL AND article_payload_size IS NOT NULL
                AND octet_length(file_key_encrypted) = 48
                AND octet_length(manifest_encrypted) >= 16
                AND verified_count = 0 AND post_copy = 'unknown' AND verify_copy = 'unknown'
                AND NOT two_spool_verified AND NOT repost_exhausted
                AND post_verified_at IS NULL AND last_retention_check IS NULL)
        );
    END IF;
END $$;
CREATE UNIQUE INDEX IF NOT EXISTS posted_files_generation_operation
    ON posted_files (generation_operation) WHERE generation_operation IS NOT NULL;

-- Owning attempts are database state, not a queue-carried attempt integer.
-- Acquisition locks the running ingest job, compares the prior attempt and
-- uses the database clock. Every takeover rotates the non-reusable token.
CREATE TABLE IF NOT EXISTS posted_file_job_attempts (
    job_id TEXT PRIMARY KEY REFERENCES ingest_jobs(id) ON DELETE RESTRICT,
    attempt BIGINT NOT NULL CHECK (attempt >= 1),
    token TEXT NOT NULL UNIQUE CHECK (token ~ '^[A-Za-z0-9_-]{1,64}$'),
    lease_until BIGINT NOT NULL CHECK (lease_until BETWEEN 0 AND 253402300799),
    UNIQUE (job_id, attempt, token)
);

CREATE TABLE IF NOT EXISTS posted_file_registry (
    plaintext_sha256 TEXT PRIMARY KEY CHECK (plaintext_sha256 ~ '^[0-9a-f]{64}$'),
    selected_generation TEXT,
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    pending_operation TEXT,
    FOREIGN KEY (selected_generation, plaintext_sha256)
        REFERENCES posted_files(id, plaintext_sha256) ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS posted_file_operations (
    id TEXT PRIMARY KEY CHECK (id ~ '^[A-Za-z0-9_-]{1,64}$'),
    plaintext_sha256 TEXT NOT NULL REFERENCES posted_file_registry(plaintext_sha256),
    generation_id TEXT NOT NULL UNIQUE,
    job_id TEXT NOT NULL REFERENCES ingest_jobs(id) ON DELETE RESTRICT,
    attempt BIGINT NOT NULL CHECK (attempt >= 1),
    attempt_token TEXT NOT NULL CHECK (attempt_token ~ '^[A-Za-z0-9_-]{1,64}$'),
    claim_token TEXT NOT NULL UNIQUE CHECK (claim_token ~ '^[A-Za-z0-9_-]{1,64}$'),
    lease_until BIGINT NOT NULL CHECK (lease_until BETWEEN 0 AND 253402300799),
    predecessor TEXT,
    predecessor_health_revision BIGINT,
    registry_revision BIGINT NOT NULL CHECK (registry_revision >= 0),
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'promoted', 'abandoned')),
    UNIQUE (id, generation_id, plaintext_sha256),
    UNIQUE (id, plaintext_sha256),
    FOREIGN KEY (generation_id, plaintext_sha256)
        REFERENCES posted_files(id, plaintext_sha256) ON DELETE RESTRICT,
    FOREIGN KEY (predecessor, plaintext_sha256)
        REFERENCES posted_files(id, plaintext_sha256) ON DELETE RESTRICT,
    CHECK ((predecessor IS NULL AND predecessor_health_revision IS NULL)
           OR (predecessor IS NOT NULL AND predecessor_health_revision IS NOT NULL
               AND predecessor_health_revision >= 0))
);
CREATE UNIQUE INDEX IF NOT EXISTS posted_file_one_pending_operation
    ON posted_file_operations(plaintext_sha256) WHERE state = 'pending';

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                   WHERE conrelid = 'public.posted_file_registry'::regclass
                     AND conname = 'posted_file_registry_pending_fkey') THEN
        ALTER TABLE posted_file_registry ADD CONSTRAINT posted_file_registry_pending_fkey
            FOREIGN KEY (pending_operation, plaintext_sha256)
            REFERENCES posted_file_operations(id, plaintext_sha256) ON DELETE RESTRICT;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                   WHERE conrelid = 'public.posted_files'::regclass
                     AND conname = 'posted_files_operation_fkey') THEN
        ALTER TABLE posted_files ADD CONSTRAINT posted_files_operation_fkey
            FOREIGN KEY (generation_operation, id, plaintext_sha256)
            REFERENCES posted_file_operations(id, generation_id, plaintext_sha256)
            DEFERRABLE INITIALLY DEFERRED;
    END IF;
END $$;

-- Mutable evidence never lives at hash scope. Unknown/transport-only stops
-- cannot authorize replacement. A new round/token invalidates old callbacks.
CREATE TABLE IF NOT EXISTS posted_file_health (
    generation_id TEXT PRIMARY KEY REFERENCES posted_files(id) ON DELETE RESTRICT,
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    round BIGINT NOT NULL DEFAULT 0 CHECK (round >= 0),
    round_token TEXT NOT NULL DEFAULT (gen_random_uuid()::TEXT)
        CHECK (round_token ~ '^[A-Za-z0-9_-]{1,64}$'),
    state TEXT NOT NULL DEFAULT 'unknown'
        CHECK (state IN ('unknown', 'verifying', 'usable', 'terminal_failed')),
    failure_kind TEXT CHECK (failure_kind IN ('verification', 'retention')),
    verified_at BIGINT CHECK (verified_at BETWEEN 0 AND 253402300799),
    retention_checked_at BIGINT CHECK (retention_checked_at BETWEEN 0 AND 253402300799),
    CHECK ((state = 'terminal_failed') = (failure_kind IS NOT NULL)),
    CHECK (state <> 'usable' OR (verified_at IS NOT NULL AND retention_checked_at IS NOT NULL))
);

-- Slot numbers address the sealed, immutable manifest order (data AND required
-- recovery articles). No article identities or plaintext plans are stored here.
CREATE TABLE IF NOT EXISTS posted_file_article_progress (
    generation_id TEXT NOT NULL REFERENCES posted_files(id) ON DELETE RESTRICT,
    round BIGINT NOT NULL CHECK (round >= 1),
    slot INTEGER NOT NULL CHECK (slot BETWEEN 0 AND 999999),
    side SMALLINT NOT NULL CHECK (side IN (0, 1)),
    confirmed BOOLEAN NOT NULL DEFAULT FALSE,
    repost_attempts INTEGER NOT NULL DEFAULT 0 CHECK (repost_attempts >= 0),
    PRIMARY KEY (generation_id, round, slot, side)
);

CREATE TABLE IF NOT EXISTS posted_file_references (
    kind TEXT NOT NULL CHECK (kind IN ('legacy_pin', 'in_flight', 'serving', 'rollback')),
    owner_id TEXT NOT NULL CHECK (owner_id ~ '^[A-Za-z0-9_-]{1,64}$'),
    generation_id TEXT NOT NULL,
    plaintext_sha256 TEXT NOT NULL,
    PRIMARY KEY (kind, owner_id, generation_id),
    FOREIGN KEY (generation_id, plaintext_sha256)
        REFERENCES posted_files(id, plaintext_sha256) ON DELETE RESTRICT
);

-- Preparation is replay-safe even after an admission: never pin managed rows
-- as legacy, reset health, or overwrite a selected generation on replay.
INSERT INTO posted_file_registry (plaintext_sha256, selected_generation)
    SELECT plaintext_sha256, id FROM posted_files WHERE generation_operation IS NULL
    ON CONFLICT (plaintext_sha256) DO NOTHING;
INSERT INTO posted_file_health (generation_id)
    SELECT id FROM posted_files ON CONFLICT (generation_id) DO NOTHING;
INSERT INTO posted_file_references (kind, owner_id, generation_id, plaintext_sha256)
    SELECT 'legacy_pin', id, id, plaintext_sha256 FROM posted_files
    WHERE generation_operation IS NULL ON CONFLICT DO NOTHING;

-- Protect every retained custody bundle. Managed generations additionally
-- reject the old ID/hash-only health writers; the new health table is the only
-- authority for their mutable state. Triggers contain only static errors.
CREATE OR REPLACE FUNCTION protect_posted_file_generation() RETURNS trigger
LANGUAGE plpgsql SET search_path = public, pg_catalog AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'posted generation custody is retained';
    END IF;
    IF (to_jsonb(NEW) - ARRAY['post_copy', 'verify_copy', 'verified_count',
            'two_spool_verified', 'repost_exhausted', 'post_verified_at', 'last_retention_check'])
       IS DISTINCT FROM
       (to_jsonb(OLD) - ARRAY['post_copy', 'verify_copy', 'verified_count',
            'two_spool_verified', 'repost_exhausted', 'post_verified_at', 'last_retention_check']) THEN
        RAISE EXCEPTION 'posted generation custody is immutable';
    END IF;
    IF OLD.generation_operation IS NOT NULL AND NEW IS DISTINCT FROM OLD THEN
        RAISE EXCEPTION 'managed generation requires fenced health writes';
    END IF;
    RETURN NEW;
END $$;
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_trigger
                   WHERE tgrelid = 'public.posted_files'::regclass
                     AND tgname = 'protect_posted_file_generation') THEN
        CREATE TRIGGER protect_posted_file_generation BEFORE UPDATE OR DELETE ON posted_files
            FOR EACH ROW EXECUTE FUNCTION protect_posted_file_generation();
    END IF;
END $$;
