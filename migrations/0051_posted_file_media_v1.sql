-- Add explicit successor media custody without relabelling historical rows.
-- Legacy envelope/ledger rows retain their exact bytes and interpretation.
-- The existing keyring, envelope-v2 payload formats and AAD tuple stay unchanged:
-- posted_files / file_key or manifest / global:posted_files / plaintext_sha256.
-- The raw per-file key remains a 32-byte payload sealed to a 48-byte GCM body.
-- Format/mode/header are immutable under protect_posted_file_generation, which
-- compares every non-health column. No health/reference/registry backfill or
-- serving, replacement, cutoff activation or R2-cleanup authority is granted.
-- Header length is not authentication: open the stored key and verify the
-- commitment with the NNTune profile and stored random mode before any read.
SET search_path = public, pg_catalog;

ALTER TABLE posted_files
    ADD COLUMN IF NOT EXISTS media_format TEXT NOT NULL DEFAULT 'legacy',
    ADD COLUMN IF NOT EXISTS media_key_mode TEXT,
    ADD COLUMN IF NOT EXISTS media_header BYTEA;

-- Keep this name so replay of historical 0037 cannot reinstall its old
-- nonce-only predicate. All original envelope completeness checks survive.
ALTER TABLE posted_files DROP CONSTRAINT IF EXISTS posted_files_crypto_complete;
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
     AND (header_nonce IS NOT NULL OR media_format = 'velmora.media.v1')
     AND file_key_encrypted IS NOT NULL
     AND file_key_envelope_version IS NOT NULL AND file_key_kek_version IS NOT NULL
     AND file_key_dek_wrapped_nonce IS NOT NULL AND file_key_dek_wrapped IS NOT NULL
     AND file_key_payload_nonce IS NOT NULL AND file_key_payload_format IS NOT NULL
     AND manifest_encrypted IS NOT NULL AND manifest_envelope_version IS NOT NULL
     AND manifest_kek_version IS NOT NULL AND manifest_dek_wrapped_nonce IS NOT NULL
     AND manifest_dek_wrapped IS NOT NULL AND manifest_payload_nonce IS NOT NULL
     AND manifest_payload_format IS NOT NULL)
);

ALTER TABLE posted_files DROP CONSTRAINT IF EXISTS posted_files_media_metadata;
ALTER TABLE posted_files ADD CONSTRAINT posted_files_media_metadata CHECK (
    (media_format = 'legacy' AND media_key_mode IS NULL AND media_header IS NULL)
    OR
    (media_format = 'velmora.media.v1' AND header_nonce IS NULL
     AND media_key_mode IS NOT NULL AND media_key_mode = 'random'
     AND media_header IS NOT NULL AND octet_length(media_header) = 48
     AND plaintext_size IS NOT NULL AND file_key_encrypted IS NOT NULL
     AND octet_length(file_key_encrypted) = 48)
);
