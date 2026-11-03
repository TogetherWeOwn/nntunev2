-- NNTune v2 migration 0029: posted-file crypto envelope + geometry (TWO-1761).
--
-- Extends the migration 0028 `posted_files` row (TWO-1768 two-spool ledger)
-- with the encrypt-on-write half of the record: the per-file key and the
-- NZB/article geometry, both sealed in the existing AES-GCM envelope v2.
-- 0028 carries copy state (backbone pair, counts, spool states); this file
-- carries key custody and geometry. One table, one writer module
-- (workers/ingest/src/posted_files.rs); the verify card owns state reuse.
--
-- Secrecy: message-IDs, subjects, file names and segment lists live ONLY
-- inside the sealed manifest envelope below — never as plaintext columns,
-- files, URLs or log lines. `header_nonce` is public (article 0 carries
-- the rclone header in the clear). Backbone labels stay labels-only
-- (0028 CHECKs); hostnames/usernames never reach the database.
--
-- Envelope layout (mirrors track_sources file_ref_*/nzb_ref_* in
-- migrations/0022_library_artifacts.sql):
-- - file_key_* seals the 32-byte per-file key. Payload format 1 = the raw
--   32-byte file key and nothing else; the key is minted per file by the
--   poster and never generated inside nntune-core.
-- - manifest_* seals the posted_manifest_v1 JSON document:
--     {"format": 1,
--      "nzb_xml_base64": "<standard-base64 NZB 1.1 document>",
--      "plaintext_ranges": [[start, end_exclusive], ...],
--      "ciphertext_offsets": [[offset, len], ...],
--      "flac_seek_index": {...} | null,
--      "par2_message_ids": ["<bare-id>", ...],
--      "posted_via": ["<backbone-label>", ...]}
--   Ranges/offsets run one entry per posted article in NZB segment order;
--   article 0's ciphertext window starts behind the 32-byte header
--   (crates/nntune-core/src/encrypt.rs `plan_articles`).
-- Unlike 0022, the payload formats ARE pinned here (CHECK = 1): these two
-- envelopes have card-fixed formats, while 0022's formats are minter-
-- assigned and intentionally open.
--
-- Envelope v2 AAD binding (ADR 0002: `domain, purpose, table, field,
-- owner_scope, row_key, envelope_version, payload_format`, plus
-- `kek_version` on the DEK-wrap AAD only; canonical length-prefixed
-- encoding, never delimiter joins). The mint generalizes
-- workers/api/src/providers.rs::aad_v2 with this tuple — NOT the v1
-- delimiter AAD (`v1|vault|<user_id>`) of vault.rs::encrypt_vault, which
-- carries no table/field/row binding and must never mint these columns:
-- - file key: domain = "nntune-envelope-v2", purpose = "payload" (or "dek"
--   for the wrap), table = "posted_files", field = "file_key",
--   owner_scope = "global:posted_files" (rows are content-global: no user
--   exists, so the scope is an explicit tagged global, never a user id or
--   an empty string), row_key = plaintext_sha256 (the only row identity
--   that exists at mint time — the DB id is assigned on write),
--   envelope_version = 2, payload_format = 1.
-- - manifest: the same tuple with field = "manifest".
-- The two envelopes share payload format 1, so file-key<->manifest
-- separation rests entirely on the AAD `field` element: cross-envelope and
-- cross-row substitution fail closed at decrypt. Pinned in code as
-- ENVELOPE_AAD_* in workers/ingest/src/posted_files.rs.
--
-- Conventions (each pinned to a precedent, not invented here):
-- - BIGINT epoch seconds with the 253402300799 ceiling (0022, 0028), not
--   timestamptz; post_verified_at/last_retention_check are NULL until the
--   verify/retention flows stamp them (0028 original_nzb_sha256 precedent
--   for NULL-able flow-stamped columns).
-- - All DDL is IF NOT EXISTS (replay-safe): the replay pass in
--   workers/api/tests/run_migration_tests.sh re-applies this file as a
--   clean no-op.

ALTER TABLE posted_files
    ADD COLUMN IF NOT EXISTS plaintext_size BIGINT
        CHECK (plaintext_size IS NULL OR plaintext_size > 0);

ALTER TABLE posted_files
    ADD COLUMN IF NOT EXISTS article_payload_size BIGINT
        CHECK (article_payload_size IS NULL OR article_payload_size > 0);

ALTER TABLE posted_files
    ADD COLUMN IF NOT EXISTS header_nonce BYTEA
        CHECK (header_nonce IS NULL OR octet_length(header_nonce) = 24);

ALTER TABLE posted_files
    ADD COLUMN IF NOT EXISTS file_key_encrypted BYTEA;

ALTER TABLE posted_files
    ADD COLUMN IF NOT EXISTS file_key_envelope_version SMALLINT
        CHECK (file_key_envelope_version IS NULL OR file_key_envelope_version = 2);

ALTER TABLE posted_files
    ADD COLUMN IF NOT EXISTS file_key_kek_version INTEGER
        CHECK (file_key_kek_version IS NULL OR file_key_kek_version >= 1);

ALTER TABLE posted_files
    ADD COLUMN IF NOT EXISTS file_key_dek_wrapped_nonce BYTEA
        CHECK (file_key_dek_wrapped_nonce IS NULL
               OR octet_length(file_key_dek_wrapped_nonce) = 12);

ALTER TABLE posted_files
    ADD COLUMN IF NOT EXISTS file_key_dek_wrapped BYTEA
        CHECK (file_key_dek_wrapped IS NULL
               OR octet_length(file_key_dek_wrapped) = 48);

ALTER TABLE posted_files
    ADD COLUMN IF NOT EXISTS file_key_payload_nonce BYTEA
        CHECK (file_key_payload_nonce IS NULL
               OR octet_length(file_key_payload_nonce) = 12);

ALTER TABLE posted_files
    ADD COLUMN IF NOT EXISTS file_key_payload_format SMALLINT
        CHECK (file_key_payload_format IS NULL OR file_key_payload_format = 1);

ALTER TABLE posted_files
    ADD COLUMN IF NOT EXISTS manifest_encrypted BYTEA;

ALTER TABLE posted_files
    ADD COLUMN IF NOT EXISTS manifest_envelope_version SMALLINT
        CHECK (manifest_envelope_version IS NULL OR manifest_envelope_version = 2);

ALTER TABLE posted_files
    ADD COLUMN IF NOT EXISTS manifest_kek_version INTEGER
        CHECK (manifest_kek_version IS NULL OR manifest_kek_version >= 1);

ALTER TABLE posted_files
    ADD COLUMN IF NOT EXISTS manifest_dek_wrapped_nonce BYTEA
        CHECK (manifest_dek_wrapped_nonce IS NULL
               OR octet_length(manifest_dek_wrapped_nonce) = 12);

ALTER TABLE posted_files
    ADD COLUMN IF NOT EXISTS manifest_dek_wrapped BYTEA
        CHECK (manifest_dek_wrapped IS NULL
               OR octet_length(manifest_dek_wrapped) = 48);

ALTER TABLE posted_files
    ADD COLUMN IF NOT EXISTS manifest_payload_nonce BYTEA
        CHECK (manifest_payload_nonce IS NULL
               OR octet_length(manifest_payload_nonce) = 12);

ALTER TABLE posted_files
    ADD COLUMN IF NOT EXISTS manifest_payload_format SMALLINT
        CHECK (manifest_payload_format IS NULL OR manifest_payload_format = 1);

ALTER TABLE posted_files
    ADD COLUMN IF NOT EXISTS post_verified_at BIGINT
        CHECK (post_verified_at IS NULL
               OR (post_verified_at >= 0 AND post_verified_at <= 253402300799));

ALTER TABLE posted_files
    ADD COLUMN IF NOT EXISTS last_retention_check BIGINT
        CHECK (last_retention_check IS NULL
               OR (last_retention_check >= 0 AND last_retention_check <= 253402300799));

-- Full crypto rows are complete: a row the poster wrote carries every
-- envelope companion and the geometry. NULL marks a pre-envelope legacy
-- row (verify/retention flows must not treat it as playable).
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'public.posted_files'::regclass
          AND conname = 'posted_files_crypto_complete'
    ) THEN
        ALTER TABLE posted_files ADD CONSTRAINT posted_files_crypto_complete CHECK (
            (plaintext_size IS NULL AND article_payload_size IS NULL
             AND header_nonce IS NULL AND file_key_encrypted IS NULL
             AND file_key_envelope_version IS NULL AND file_key_kek_version IS NULL
             AND file_key_dek_wrapped_nonce IS NULL AND file_key_dek_wrapped IS NULL
             AND file_key_payload_nonce IS NULL AND file_key_payload_format IS NULL
             AND manifest_encrypted IS NULL AND manifest_envelope_version IS NULL
             AND manifest_kek_version IS NULL AND manifest_dek_wrapped_nonce IS NULL
             AND manifest_dek_wrapped IS NULL AND manifest_payload_nonce IS NULL
             AND manifest_payload_format IS NULL)
            OR
            (plaintext_size IS NOT NULL AND article_payload_size IS NOT NULL
             AND header_nonce IS NOT NULL AND file_key_encrypted IS NOT NULL
             AND file_key_envelope_version IS NOT NULL AND file_key_kek_version IS NOT NULL
             AND file_key_dek_wrapped_nonce IS NOT NULL AND file_key_dek_wrapped IS NOT NULL
             AND file_key_payload_nonce IS NOT NULL AND file_key_payload_format IS NOT NULL
             AND manifest_encrypted IS NOT NULL AND manifest_envelope_version IS NOT NULL
             AND manifest_kek_version IS NOT NULL AND manifest_dek_wrapped_nonce IS NOT NULL
             AND manifest_dek_wrapped IS NOT NULL AND manifest_payload_nonce IS NOT NULL
             AND manifest_payload_format IS NOT NULL)
        );
    END IF;
END $$;
