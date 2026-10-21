-- NNTune v2 migration 0021: library + artifact tables (ADR 0002 / ADR 0003).
--
-- PlanetScale Postgres via Hyperdrive. All access uses parameterized
-- statements (never string-interpolated). First consumers are the ingest
-- `opus-64` publication statements in workers/ingest/src/transcode.rs:
-- cache_objects row + per-user track_media row (format='opus',
-- bitrate=64) + user_cache_grants(user, artifact_hash) with the fetching
-- provider_id.
--
-- Conventions (each pinned to a precedent, not invented here):
-- - Timestamps are BIGINT epoch seconds like the rest of the applied
--   auth/ledger schema (migrations/0019_native_rest_credentials.sql),
--   not the ADR's timestamptz line: the store layer binds i64
--   throughout. The account-deletion fence still reads the timestamptz
--   users.deleting_at from 0006_account_deletion.sql.
-- - ingest_jobs.id is the pipeline job_id string verbatim (TEXT, the
--   jobs::validate_job_id alphabet), not a generated UUID: job ids are
--   pre-existing pipeline identities like users.id, and track_sources /
--   track_media carry them as source_job_id. A future local-catalog
--   migration types artists/releases.local_job_id to match.
-- - tracks.release_id and track_sources.indexer_id are shaped final
--   (UUID NOT NULL / UUID NULL) but carry no FOREIGN KEY yet: the
--   releases and indexers tables do not exist. Their migrations add the
--   constraints; no backfill beyond the stored ids is needed.
-- - cache_objects.created_at, track_media.last_served_at and
--   user_cache_grants.last_served_at default to the DB clock because
--   the publication statements do not bind them; the defaults
--   initialize the ADR 0003 retention/idle clock at publication.
-- - cache_objects.ref_count is advisory (ADR 0003: never the sweep
--   mark set); NULL means untracked.
-- - Every FK is RESTRICT (ADR 0002 rule 3): library/account deletion
--   is an explicit fenced batched procedure, never a bare cascade.
-- - Publication order falls out of the FKs: the content-global
--   cache_objects row (no user identity, no fence) may publish as soon
--   as bytes verify; track_media / user_cache_grants rows publish only
--   once their tracks row exists (at or after ingest Writing).
-- - No GRANTs: staging/prod rely on database default privileges and
--   the role names are host-owned (docs/ops.md "Database roles and
--   migrator wiring"). The migration owner re-tests the role matrix at
--   deploy.

CREATE TABLE IF NOT EXISTS ingest_jobs (
    id              TEXT PRIMARY KEY CHECK (id ~ '^[A-Za-z0-9_-]{1,64}$'),
    user_id         TEXT NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    idempotency_key TEXT NOT NULL CHECK (char_length(idempotency_key) BETWEEN 1 AND 768),
    accepted_at     BIGINT NOT NULL CHECK (accepted_at >= 0 AND accepted_at <= 253402300799),
    status          TEXT NOT NULL CHECK (status IN ('queued', 'running', 'done', 'failed')),
    UNIQUE (user_id, idempotency_key)
);

CREATE TABLE IF NOT EXISTS providers (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id                 TEXT NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    label                   TEXT NOT NULL CHECK (char_length(label) <= 64),
    encrypted_creds         BYTEA NOT NULL,
    creds_envelope_version  SMALLINT NOT NULL CHECK (creds_envelope_version = 2),
    creds_kek_version       INTEGER NOT NULL,
    creds_dek_wrapped_nonce BYTEA NOT NULL CHECK (octet_length(creds_dek_wrapped_nonce) = 12),
    creds_dek_wrapped       BYTEA NOT NULL CHECK (octet_length(creds_dek_wrapped) = 48),
    creds_payload_nonce     BYTEA NOT NULL CHECK (octet_length(creds_payload_nonce) = 12),
    creds_payload_format    SMALLINT NOT NULL,
    created_at              BIGINT NOT NULL CHECK (created_at >= 0 AND created_at <= 253402300799),
    last_verified_at        BIGINT NOT NULL CHECK (last_verified_at >= 0 AND last_verified_at <= 253402300799),
    UNIQUE (user_id, id)
);

CREATE TABLE IF NOT EXISTS cache_objects (
    content_hash    TEXT PRIMARY KEY CHECK (content_hash ~ '^[0-9a-f]{64}$'),
    byte_size       BIGINT NOT NULL CHECK (byte_size >= 0),
    r2_bucket       TEXT NOT NULL CHECK (char_length(r2_bucket) BETWEEN 1 AND 63),
    r2_key          TEXT NOT NULL CHECK (char_length(r2_key) BETWEEN 1 AND 1024),
    ref_count       BIGINT NULL CHECK (ref_count IS NULL OR ref_count >= 0),
    created_at      BIGINT NOT NULL DEFAULT (extract(epoch from now())::bigint)
                    CHECK (created_at >= 0 AND created_at <= 253402300799),
    last_verified_at BIGINT NOT NULL CHECK (last_verified_at >= 0 AND last_verified_at <= 253402300799)
);

CREATE TABLE IF NOT EXISTS tracks (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    mbid        UUID NULL,
    release_id  UUID NOT NULL,
    title       TEXT NOT NULL,
    medium_no   INTEGER NOT NULL CHECK (medium_no >= 1),
    track_no    INTEGER NOT NULL CHECK (track_no >= 1),
    duration_ms INTEGER NULL CHECK (duration_ms IS NULL OR duration_ms > 0),
    UNIQUE (release_id, medium_no, track_no)
);
CREATE INDEX IF NOT EXISTS idx_tracks_mbid ON tracks (mbid);

CREATE TABLE IF NOT EXISTS track_sources (
    user_id                 TEXT NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    track_id                UUID NOT NULL REFERENCES tracks (id) ON DELETE RESTRICT,
    source_job_id           TEXT NOT NULL REFERENCES ingest_jobs (id) ON DELETE RESTRICT,
    indexer_id              UUID NULL,
    nzb_ref_encrypted       BYTEA NOT NULL,
    nzb_ref_envelope_version SMALLINT NOT NULL CHECK (nzb_ref_envelope_version = 2),
    nzb_ref_kek_version     INTEGER NOT NULL,
    nzb_ref_dek_wrapped_nonce BYTEA NOT NULL CHECK (octet_length(nzb_ref_dek_wrapped_nonce) = 12),
    nzb_ref_dek_wrapped     BYTEA NOT NULL CHECK (octet_length(nzb_ref_dek_wrapped) = 48),
    nzb_ref_payload_nonce   BYTEA NOT NULL CHECK (octet_length(nzb_ref_payload_nonce) = 12),
    nzb_ref_payload_format  SMALLINT NOT NULL,
    PRIMARY KEY (user_id, track_id)
);
CREATE INDEX IF NOT EXISTS idx_track_sources_track ON track_sources (track_id);
CREATE INDEX IF NOT EXISTS idx_track_sources_job ON track_sources (source_job_id);

CREATE TABLE IF NOT EXISTS track_media (
    user_id         TEXT NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    track_id        UUID NOT NULL REFERENCES tracks (id) ON DELETE RESTRICT,
    source_job_id   TEXT NOT NULL REFERENCES ingest_jobs (id) ON DELETE RESTRICT,
    content_hash    TEXT NOT NULL REFERENCES cache_objects (content_hash) ON DELETE RESTRICT,
    format          TEXT NOT NULL,
    bitrate         INTEGER NOT NULL CHECK (bitrate > 0),
    verified_at     BIGINT NOT NULL CHECK (verified_at >= 0 AND verified_at <= 253402300799),
    last_served_at  BIGINT NOT NULL DEFAULT (extract(epoch from now())::bigint)
                    CHECK (last_served_at >= 0 AND last_served_at <= 253402300799),
    PRIMARY KEY (user_id, track_id, source_job_id, content_hash)
);
CREATE INDEX IF NOT EXISTS idx_track_media_track ON track_media (track_id);
CREATE INDEX IF NOT EXISTS idx_track_media_job ON track_media (source_job_id);
CREATE INDEX IF NOT EXISTS idx_track_media_hash ON track_media (content_hash);

CREATE TABLE IF NOT EXISTS user_cache_grants (
    user_id         TEXT NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    content_hash    TEXT NOT NULL REFERENCES cache_objects (content_hash) ON DELETE RESTRICT,
    provider_id     UUID NOT NULL,
    verified_at     BIGINT NOT NULL CHECK (verified_at >= 0 AND verified_at <= 253402300799),
    last_served_at  BIGINT NOT NULL DEFAULT (extract(epoch from now())::bigint)
                    CHECK (last_served_at >= 0 AND last_served_at <= 253402300799),
    PRIMARY KEY (user_id, content_hash),
    FOREIGN KEY (user_id, provider_id) REFERENCES providers (user_id, id) ON DELETE RESTRICT
);
CREATE INDEX IF NOT EXISTS idx_user_cache_grants_hash ON user_cache_grants (content_hash);
