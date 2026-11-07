-- Service pool provider registry (ADR 0014 foundation).
--
-- pool_provider_secrets holds one pool account password under the v2
-- field envelope (same column shapes and CHECKs as providers in 0022:
-- fresh per-row DEK, 12-byte nonces, 48-byte wrapped DEK, envelope
-- version 2 literal). The envelope AAD binds table
-- `pool_provider_secrets`, owner scope `service` and the row key
-- `secret_ref`, so it never authenticates as a legacy providers row.
--
-- pool_providers holds the non-secret settings and a revision counter
-- for cache invalidation. Account ids are UUID.
--
-- user_cache_grants.provider_id used to reference the per-user
-- providers (user_id, id) composite. Attribution now points at
-- pool_providers(id) (UUID). Existing values are NOT discarded: they move to
-- the new legacy_provider_id column, which keeps the original composite
-- providers FK. legacy_provider_id is retained until the follow-up that
-- purges legacy per-user credentials; until then it is the only copy of
-- the old attribution and keeps those providers rows undeletable
-- (RESTRICT). Every grant row is preserved; provider_id is nulled only
-- after its value is copied, and stays UUID.
--
-- Replay-safe: every statement is guarded (IF NOT EXISTS, pg_constraint
-- and information_schema checks), so re-applying changes nothing and
-- never re-nulls copied data (workers/api/tests/run_migration_tests.sh
-- re-applies every migration).

SET search_path = public, pg_catalog;

CREATE TABLE IF NOT EXISTS public.pool_provider_secrets (
    secret_ref              TEXT PRIMARY KEY
                            CHECK (secret_ref ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'),
    encrypted_creds         BYTEA NOT NULL,
    creds_envelope_version  SMALLINT NOT NULL CHECK (creds_envelope_version = 2),
    creds_kek_version       INTEGER NOT NULL,
    creds_dek_wrapped_nonce BYTEA NOT NULL CHECK (octet_length(creds_dek_wrapped_nonce) = 12),
    creds_dek_wrapped       BYTEA NOT NULL CHECK (octet_length(creds_dek_wrapped) = 48),
    creds_payload_nonce     BYTEA NOT NULL CHECK (octet_length(creds_payload_nonce) = 12),
    creds_payload_format    SMALLINT NOT NULL,
    created_at              BIGINT NOT NULL CHECK (created_at >= 0 AND created_at <= 253402300799),
    updated_at              BIGINT NOT NULL CHECK (updated_at >= 0 AND updated_at <= 253402300799)
);

CREATE TABLE IF NOT EXISTS public.pool_providers (
    id              UUID PRIMARY KEY,
    label           TEXT NOT NULL CHECK (char_length(label) BETWEEN 1 AND 64),
    host            TEXT NOT NULL CHECK (char_length(host) BETWEEN 1 AND 253),
    port            INTEGER NOT NULL CHECK (port IN (119, 563)),
    tls             BOOLEAN NOT NULL CHECK (tls),
    username        TEXT NOT NULL CHECK (char_length(username) BETWEEN 1 AND 255),
    can_read        BOOLEAN NOT NULL,
    can_post        BOOLEAN NOT NULL,
    max_connections INTEGER NOT NULL CHECK (max_connections BETWEEN 1 AND 50),
    stream_reserve  INTEGER NOT NULL CHECK (stream_reserve BETWEEN 0 AND max_connections),
    ingest_limit    INTEGER NOT NULL CHECK (ingest_limit >= 0),
    backbone        TEXT NOT NULL CHECK (char_length(backbone) BETWEEN 1 AND 64),
    tier            INTEGER NOT NULL CHECK (tier BETWEEN 0 AND 100),
    enabled         BOOLEAN NOT NULL,
    secret_ref      TEXT NOT NULL REFERENCES public.pool_provider_secrets (secret_ref) ON DELETE RESTRICT,
    revision        BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at      BIGINT NOT NULL CHECK (created_at >= 0 AND created_at <= 253402300799),
    updated_at      BIGINT NOT NULL CHECK (updated_at >= 0 AND updated_at <= 253402300799),
    CHECK (NOT can_read OR (stream_reserve >= 1 AND ingest_limit <= max_connections - stream_reserve)),
    CHECK (can_read OR ingest_limit = 0)
);
CREATE INDEX IF NOT EXISTS idx_pool_providers_secret_ref
    ON public.pool_providers (secret_ref);

-- One-time attribution move, a single DO block so it is atomic and
-- guarded by one fact: the original unnamed-in-0022 composite FK
-- (user_cache_grants_user_id_provider_id_fkey) still exists. While it
-- exists, provider_id still holds the only copy of the legacy
-- attribution: copy it into legacy_provider_id, add the composite
-- providers FK on the legacy column (validated against the copied
-- values) before dropping the old FK so the guarantee never lapses,
-- then drop NOT NULL and null provider_id. The pool FK is added only
-- when absent. On re-apply the original FK is gone, so nothing is
-- copied or nulled again and any new pool attribution is untouched.
-- provider_id stays UUID.
DO $$
BEGIN
    ALTER TABLE public.user_cache_grants
        ADD COLUMN IF NOT EXISTS legacy_provider_id UUID NULL;

    IF EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'public.user_cache_grants'::regclass
          AND conname = 'user_cache_grants_user_id_provider_id_fkey'
    ) THEN
        UPDATE public.user_cache_grants
           SET legacy_provider_id = provider_id
         WHERE legacy_provider_id IS NULL AND provider_id IS NOT NULL;

        IF NOT EXISTS (
            SELECT 1 FROM pg_constraint
            WHERE conrelid = 'public.user_cache_grants'::regclass
              AND conname = 'user_cache_grants_legacy_provider_fkey'
        ) THEN
            ALTER TABLE public.user_cache_grants
                ADD CONSTRAINT user_cache_grants_legacy_provider_fkey
                FOREIGN KEY (user_id, legacy_provider_id)
                REFERENCES public.providers (user_id, id) ON DELETE RESTRICT;
        END IF;

        ALTER TABLE public.user_cache_grants
            DROP CONSTRAINT user_cache_grants_user_id_provider_id_fkey;
        ALTER TABLE public.user_cache_grants
            ALTER COLUMN provider_id DROP NOT NULL;
        UPDATE public.user_cache_grants SET provider_id = NULL;
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'public.user_cache_grants'::regclass
          AND conname = 'user_cache_grants_pool_provider_fkey'
    ) THEN
        ALTER TABLE public.user_cache_grants
            ADD CONSTRAINT user_cache_grants_pool_provider_fkey
            FOREIGN KEY (provider_id)
            REFERENCES public.pool_providers (id) ON DELETE RESTRICT;
    END IF;
END
$$;

CREATE INDEX IF NOT EXISTS idx_user_cache_grants_pool_provider
    ON public.user_cache_grants (provider_id);
