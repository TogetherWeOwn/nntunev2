-- ADR 0002/0006 credentials, separate from the unmounted legacy 0023 table.
-- No legacy ciphertext/HMAC is promoted: it lacks the required version and AAD.
-- Provisioning remains an explicit, separately authorized operation.
SET search_path = public, pg_catalog;

CREATE TABLE IF NOT EXISTS public.subsonic_credentials (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL REFERENCES public.users (id) ON DELETE CASCADE,
    label TEXT NOT NULL DEFAULT '' CHECK (octet_length(label) <= 128),
    subsonic_username TEXT NOT NULL UNIQUE CHECK (
        octet_length(subsonic_username) BETWEEN 1 AND 128
        AND subsonic_username ~ '^[A-Za-z0-9_-]+$'
    ),
    api_key_hmac TEXT,
    pepper_version INTEGER,
    token_secret_encrypted BYTEA,
    token_envelope_version SMALLINT,
    token_payload_format SMALLINT,
    token_kek_version INTEGER,
    token_dek_wrapped_nonce BYTEA,
    token_dek_wrapped BYTEA,
    token_payload_nonce BYTEA,
    auth_mode TEXT NOT NULL DEFAULT 'apikey' CHECK (auth_mode IN ('apikey', 'token', 'both')),
    scope TEXT NOT NULL DEFAULT 'subsonic' CHECK (scope = 'subsonic'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    attempt_window BIGINT NOT NULL DEFAULT 0 CHECK (attempt_window >= 0),
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count BETWEEN 0 AND 60),
    UNIQUE (pepper_version, api_key_hmac),
    CHECK (
        (auth_mode IN ('apikey', 'both')
            AND api_key_hmac IS NOT NULL AND api_key_hmac ~ '^[0-9a-f]{64}$'
            AND pepper_version IS NOT NULL AND pepper_version > 0)
        OR (auth_mode = 'token' AND api_key_hmac IS NULL AND pepper_version IS NULL)
    ),
    CHECK (
        (auth_mode IN ('token', 'both')
            AND token_secret_encrypted IS NOT NULL
            AND octet_length(token_secret_encrypted) BETWEEN 48 AND 528
            AND token_envelope_version IS NOT NULL AND token_envelope_version = 2
            AND token_payload_format IS NOT NULL AND token_payload_format = 1
            AND token_kek_version IS NOT NULL AND token_kek_version > 0
            AND token_dek_wrapped_nonce IS NOT NULL AND octet_length(token_dek_wrapped_nonce) = 12
            AND token_dek_wrapped IS NOT NULL AND octet_length(token_dek_wrapped) = 48
            AND token_payload_nonce IS NOT NULL AND octet_length(token_payload_nonce) = 12)
        OR (auth_mode = 'apikey'
            AND token_secret_encrypted IS NULL AND token_envelope_version IS NULL
            AND token_payload_format IS NULL AND token_kek_version IS NULL
            AND token_dek_wrapped_nonce IS NULL AND token_dek_wrapped IS NULL
            AND token_payload_nonce IS NULL)
    )
);
CREATE INDEX IF NOT EXISTS subsonic_credentials_user_id_idx ON public.subsonic_credentials (user_id);
-- Dormant credentials count too: retirement never relies on last_used_at.
CREATE INDEX IF NOT EXISTS subsonic_credentials_live_pepper_idx
    ON public.subsonic_credentials (pepper_version) WHERE revoked_at IS NULL;
