-- Per-client Subsonic credentials (PlanetScale Postgres via Hyperdrive).
--
-- Mirrors v1 (`SubsonicKeySeal` + `ApiKeyAuthenticator`): the `apiKey`
-- parameter is looked up by an HMAC-SHA256 index keyed with a pepper that
-- lives only in the worker secret `SUBSONIC_INDEX_PEPPER` (never in git),
-- so a database dump alone cannot test candidate keys. The peppered HMAC
-- replaces the bare unsalted SHA-256: same lookup shape, strictly stronger.
--
-- Salted token auth (`u`+`t`+`s`, the Subsonic default since API 1.13)
-- needs the server to recompute md5(secret + salt) for a client-chosen
-- salt, so it needs the secret's plaintext. Secrets are therefore sealed
-- with XChaCha20-Poly1305 under a KMS-held key ONLY for credentials that
-- opt into legacy auth (`legacy_enabled`); apiKey-only clients store no
-- ciphertext at all. The biconditional CHECK enforces that pairing.
--
-- The 44 path also checks `revoked_at` and the owning user's disabled
-- state (users table, joined at query time): a revoked key, or a key whose
-- owner is disabled, answers 44 like v1.

CREATE TABLE IF NOT EXISTS subsonic_clients (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    client_name TEXT NOT NULL DEFAULT '',
    -- HMAC-SHA256(api_key, pepper) hex; pepper from SUBSONIC_INDEX_PEPPER.
    api_key_hmac TEXT NOT NULL UNIQUE,
    -- Sealed legacy secret for u+p / u+t+s auth; NULL unless legacy_enabled.
    key_ct TEXT,
    key_nonce TEXT,
    key_version INT NOT NULL DEFAULT 1,
    legacy_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    CONSTRAINT subsonic_clients_legacy_seal_check CHECK (
        (legacy_enabled AND key_ct IS NOT NULL AND key_nonce IS NOT NULL)
        OR (NOT legacy_enabled AND key_ct IS NULL AND key_nonce IS NULL)
    )
);

CREATE INDEX IF NOT EXISTS subsonic_clients_user_id_idx
    ON subsonic_clients (user_id);
CREATE INDEX IF NOT EXISTS subsonic_clients_revoked_idx
    ON subsonic_clients (revoked_at) WHERE revoked_at IS NOT NULL;
