-- Per-client Subsonic credentials (PlanetScale Postgres via Hyperdrive).
--
-- v1 authenticates Subsonic clients against per-user API keys
-- (`users.subsonic_api_key`); v2 issues one key per client app so a key can
-- be revoked per device without rotating the user's other clients. Only the
-- SHA-256 hash is stored — never the key itself, never any secret in git.

CREATE TABLE IF NOT EXISTS subsonic_clients (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL,
    client_name TEXT NOT NULL DEFAULT '',
    api_key_hash TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS subsonic_clients_user_id_idx
    ON subsonic_clients (user_id);
