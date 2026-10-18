-- 0019_native_rest_credentials: scoped native REST credentials (ADR 0010).
--
-- New table only. No applied migration is edited; no Subsonic, session, or
-- vault row changes. The display secret (`ntk_<base62>`) is shown once at
-- issuance and never stored: what persists is the HMAC-SHA256 of the key
-- plus a short non-secret prefix for "which key is this" listing, keyed by
-- the exact pepper version in a dedicated NATIVE_REST_PEPPER keyring
-- (composite UNIQUE (pepper_version, key_hmac), dual-version lookup during
-- a transition). Timestamps are BIGINT epoch seconds like the rest of the
-- auth schema (users/sessions/magic_link_tokens), not timestamptz: the
-- ADR's timestamptz line describes conventions, the applied schema uses
-- BIGINT, and the store layer binds i64 throughout.
--
-- Account erasure deletes these rows before deleting the user alongside the
-- other FK children (ON DELETE CASCADE, same as sessions and Subsonic
-- credentials). No backfill, no plaintext handling.

CREATE TABLE IF NOT EXISTS native_rest_credentials (
    id              TEXT PRIMARY KEY,
    user_id         TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    label           TEXT NOT NULL DEFAULT '',
    key_prefix      TEXT NOT NULL,
    key_hmac        TEXT NOT NULL,
    pepper_version  INTEGER NOT NULL,
    UNIQUE (pepper_version, key_hmac),
    scopes          TEXT[] NOT NULL,
    created_at      BIGINT NOT NULL,
    expires_at      BIGINT NOT NULL,
    last_used_at    BIGINT,
    revoked_at      BIGINT,
    created_from_ip TEXT NOT NULL,
    CHECK (char_length(label) <= 64)
);
CREATE INDEX IF NOT EXISTS idx_native_rest_credentials_user
    ON native_rest_credentials (user_id);
