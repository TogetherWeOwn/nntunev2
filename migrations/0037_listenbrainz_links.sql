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

-- One fleet-wide provider budget, not one per account/token or isolate.
-- No credentials, user identifiers or response metadata are stored here.
CREATE TABLE IF NOT EXISTS public.listenbrainz_outbound_rate (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    next_allowed_at_ms BIGINT NOT NULL CHECK (next_allowed_at_ms >= 0),
    in_flight BOOLEAN NOT NULL DEFAULT FALSE,
    generation BIGINT NOT NULL DEFAULT 0 CHECK (generation >= 0)
);
