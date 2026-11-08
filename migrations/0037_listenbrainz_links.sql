-- Encrypted, optional ListenBrainz credential; no provider calls or backfill.
SET search_path = public, pg_catalog;

CREATE TABLE IF NOT EXISTS public.listenbrainz_links (
    user_id TEXT PRIMARY KEY REFERENCES public.users(id) ON DELETE CASCADE,
    username TEXT NOT NULL CHECK (char_length(username) BETWEEN 1 AND 64),
    kek_version INTEGER NOT NULL CHECK (kek_version > 0),
    dek_wrapped_nonce BYTEA NOT NULL CHECK (octet_length(dek_wrapped_nonce) = 12),
    dek_wrapped BYTEA NOT NULL CHECK (octet_length(dek_wrapped) = 48),
    payload_nonce BYTEA NOT NULL CHECK (octet_length(payload_nonce) = 12),
    ciphertext BYTEA NOT NULL CHECK (octet_length(ciphertext) = 52),
    created_at BIGINT NOT NULL CHECK (created_at >= 0)
);
