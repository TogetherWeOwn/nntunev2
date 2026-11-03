-- Admin NZB ingest (originals ledger): per-file POST acceptance with a
-- content-derived job identity and an encrypted no-expiry original.
--
-- ingest_jobs gains an admin-ledger projection: `source` discriminates
-- pipeline rows (`pipeline`, the default every pre-existing row takes)
-- from admin-upload rows (`admin`). Admin rows carry the upload's
-- content identity and validated shape; `user_id` stays the uploading
-- admin for the FK and the write fence only — the job identity and the
-- idempotency key derive from the content hash, never the uploader, so
-- two admins uploading the same bytes converge on one job.
--
-- ingest_originals holds the accepted NZB bytes under the v2 field
-- envelope (same column shapes and CHECKs as the track_sources
-- `nzb_ref_*` envelope in migration 0022: fresh per-row DEK, 12-byte
-- nonces, 48-byte wrapped DEK, envelope version 2 literal). There is
-- deliberately no expiry column, no KV/R2 binding, and no read path
-- back to plaintext outside the store-level decrypt used by tests:
-- reads serve the redacted job view only.
--
-- Replay-safe: every statement is guarded (ADD COLUMN IF NOT EXISTS,
-- CREATE TABLE/INDEX IF NOT EXISTS, pg_constraint-guarded ADD
-- CONSTRAINT), so re-applying this file changes nothing
-- (workers/api/tests/run_migration_tests.sh re-applies every
-- migration).

ALTER TABLE ingest_jobs ADD COLUMN IF NOT EXISTS source TEXT NOT NULL DEFAULT 'pipeline'
    CHECK (source IN ('pipeline', 'admin'));
ALTER TABLE ingest_jobs ADD COLUMN IF NOT EXISTS content_sha256 TEXT
    CHECK (content_sha256 IS NULL OR content_sha256 ~ '^[0-9a-f]{64}$');
ALTER TABLE ingest_jobs ADD COLUMN IF NOT EXISTS nzb_bytes BIGINT
    CHECK (nzb_bytes IS NULL OR nzb_bytes >= 0);
ALTER TABLE ingest_jobs ADD COLUMN IF NOT EXISTS file_count INTEGER
    CHECK (file_count IS NULL OR file_count >= 0);
ALTER TABLE ingest_jobs ADD COLUMN IF NOT EXISTS segment_count INTEGER
    CHECK (segment_count IS NULL OR segment_count >= 0);
ALTER TABLE ingest_jobs ADD COLUMN IF NOT EXISTS failure_reason TEXT
    CHECK (failure_reason IS NULL OR failure_reason IN
        ('empty', 'malformed', 'not_nzb', 'too_large',
         'too_many_files', 'too_many_segments', 'declared_bytes_over_cap'));

-- Admin-row shape coupling (pipeline rows always satisfy the first
-- disjunct via the `pipeline` default): an admin row always names its
-- content and size; a failed admin row names its failure reason; an
-- accepted admin row carries no failure reason and names its validated
-- file/segment counts (the original row holds the bytes).
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'public.ingest_jobs'::regclass
          AND conname = 'ingest_jobs_admin_content_check'
    ) THEN
        ALTER TABLE ingest_jobs ADD CONSTRAINT ingest_jobs_admin_content_check CHECK (
            source = 'pipeline'
            OR (content_sha256 IS NOT NULL AND nzb_bytes IS NOT NULL
                AND (status <> 'failed' OR failure_reason IS NOT NULL)
                AND (status = 'failed'
                     OR (failure_reason IS NULL
                         AND file_count IS NOT NULL
                         AND segment_count IS NOT NULL)))
        );
    END IF;
END
$$;

-- Backstop for the content-derived job identity: the writer inserts by
-- deterministic id with ON CONFLICT (id) DO NOTHING (convergence), and
-- this partial index refuses a second admin row for one content hash
-- even if a caller ever derived a different id for the same bytes.
CREATE UNIQUE INDEX IF NOT EXISTS ingest_jobs_admin_content_sha256_uidx
    ON ingest_jobs (content_sha256)
    WHERE source = 'admin' AND content_sha256 IS NOT NULL;

CREATE TABLE IF NOT EXISTS ingest_originals (
    job_id                  TEXT PRIMARY KEY REFERENCES ingest_jobs (id) ON DELETE RESTRICT,
    content_sha256          TEXT NOT NULL CHECK (content_sha256 ~ '^[0-9a-f]{64}$'),
    nzb_bytes               BIGINT NOT NULL CHECK (nzb_bytes >= 0),
    nzb_ref_encrypted       BYTEA NOT NULL,
    nzb_ref_envelope_version SMALLINT NOT NULL CHECK (nzb_ref_envelope_version = 2),
    nzb_ref_kek_version     INTEGER NOT NULL,
    nzb_ref_dek_wrapped_nonce BYTEA NOT NULL CHECK (octet_length(nzb_ref_dek_wrapped_nonce) = 12),
    nzb_ref_dek_wrapped     BYTEA NOT NULL CHECK (octet_length(nzb_ref_dek_wrapped) = 48),
    nzb_ref_payload_nonce   BYTEA NOT NULL CHECK (octet_length(nzb_ref_payload_nonce) = 12),
    nzb_ref_payload_format  SMALLINT NOT NULL,
    created_at              BIGINT NOT NULL CHECK (created_at >= 0 AND created_at <= 253402300799)
);
