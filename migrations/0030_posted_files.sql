-- NNTune v2 migration 0030: two-spool posted-file ledger (TWO-1768).
--
-- One content-global row per tagged plaintext sha256, recording the
-- first-spool post and the second-spool (stage B2.5) verification state.
-- The second copy reuses the same message-IDs on a different backbone —
-- no second NZB, no second ID set — so this row carries hashes, backbone
-- labels and counts only: never message-IDs, subjects, file names or
-- segment lists (those live in the NZB bytes, not the database).
--
-- Conventions (each pinned to a precedent, not invented here):
-- - Content-global row with no user identity and no writer fence, like
--   cache_objects (migrations/0022_library_artifacts.sql): the dedupe key
--   is the plaintext sha256, so every uploader of the same bytes
--   converges on one row.
-- - id is a TEXT uuid default verbatim like ingest_jobs.id (0022), not a
--   generated UUID column: pipeline writers bind nothing for it and the
--   dedupe lookup returns it as an opaque handle.
-- - Timestamps are BIGINT epoch seconds with the 253402300799 ceiling
--   like the rest of the applied schema (0022), not timestamptz; rows are
--   stamped by the DB clock because the upsert does not bind them.
-- - Backbone labels pin the ingest pool-config vocabulary
--   (workers/ingest/src/two_spool.rs `validate_backbone_label`):
--   1..=64 lowercase alphanumerics and dashes — never a hostname.
--   Hostnames/usernames are vault secrets and never reach the database.
-- - Copy states pin the `SpoolCopyState` snake_case alphabet
--   (unknown/present/missing/reposted/verified) as a CHECKed IN list,
--   like ingest_jobs.status (0022).
-- - The backbone pair CHECK (verify <> post) encodes the two-spool rule
--   that accounts sharing a backbone count as one copy: the verify lane
--   must be a different backbone, or the "second copy" is the same spool
--   counted twice.
-- - All DDL is IF NOT EXISTS (replay-safe): the replay pass in
--   workers/api/tests/run_migration_tests.sh re-applies this file as a
--   clean no-op.

CREATE TABLE IF NOT EXISTS posted_files (
    id                  TEXT PRIMARY KEY DEFAULT (gen_random_uuid()::TEXT)
                        CHECK (id ~ '^[A-Za-z0-9_-]{1,64}$'),
    plaintext_sha256    TEXT NOT NULL UNIQUE
                        CHECK (plaintext_sha256 ~ '^[0-9a-f]{64}$'),
    backbone_post       TEXT NOT NULL
                        CHECK (backbone_post ~ '^[a-z0-9-]{1,64}$'),
    backbone_verify     TEXT NOT NULL
                        CHECK (backbone_verify ~ '^[a-z0-9-]{1,64}$'),
    CHECK (backbone_verify <> backbone_post),
    article_count       INTEGER NOT NULL
                        CHECK (article_count >= 1 AND article_count <= 1000000),
    verified_count      INTEGER NOT NULL DEFAULT 0
                        CHECK (verified_count >= 0 AND verified_count <= 1000000),
    original_nzb_sha256 TEXT NULL
                        CHECK (original_nzb_sha256 IS NULL
                               OR original_nzb_sha256 ~ '^[0-9a-f]{64}$'),
    post_copy           TEXT NOT NULL DEFAULT 'unknown'
                        CHECK (post_copy IN ('unknown', 'present', 'missing', 'reposted', 'verified')),
    verify_copy         TEXT NOT NULL DEFAULT 'unknown'
                        CHECK (verify_copy IN ('unknown', 'present', 'missing', 'reposted', 'verified')),
    two_spool_verified  BOOLEAN NOT NULL DEFAULT FALSE,
    repost_exhausted    BOOLEAN NOT NULL DEFAULT FALSE,
    created_at          BIGINT NOT NULL DEFAULT (extract(epoch from now())::bigint)
                        CHECK (created_at >= 0 AND created_at <= 253402300799)
);
