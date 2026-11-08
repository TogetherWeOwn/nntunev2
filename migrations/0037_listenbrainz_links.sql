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

-- Independent bounded keyset sweeps; finished_at is not key-retirement proof.
-- This cursor is restricted account-linked state, never telemetry.
CREATE TABLE IF NOT EXISTS public.listenbrainz_rotation_runs (
    id TEXT PRIMARY KEY CHECK (id ~ '^[A-Za-z0-9_-]{1,128}$'),
    env TEXT NOT NULL CHECK (env ~ '^[A-Za-z0-9_-]{1,64}$'),
    to_version INTEGER NOT NULL CHECK (to_version > 0),
    owner_cursor TEXT CHECK (owner_cursor ~ '^[A-Za-z0-9_-]{1,64}$'),
    checkpoint_seq BIGINT NOT NULL DEFAULT 0 CHECK (checkpoint_seq >= 0),
    scanned BIGINT NOT NULL DEFAULT 0 CHECK (scanned >= 0),
    rewrapped BIGINT NOT NULL DEFAULT 0 CHECK (rewrapped >= 0),
    no_link BIGINT NOT NULL DEFAULT 0 CHECK (no_link >= 0),
    already_current BIGINT NOT NULL DEFAULT 0 CHECK (already_current >= 0),
    deferred BIGINT NOT NULL DEFAULT 0 CHECK (deferred >= 0),
    failed BIGINT NOT NULL DEFAULT 0 CHECK (failed >= 0),
    started_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    finished_at TIMESTAMPTZ,
    CHECK (scanned::numeric = rewrapped::numeric + no_link::numeric
        + already_current::numeric + deferred::numeric + failed::numeric)
);
CREATE UNIQUE INDEX IF NOT EXISTS listenbrainz_rotation_one_open_env
    ON public.listenbrainz_rotation_runs (env) WHERE finished_at IS NULL;
