-- NNTune v2 migration 0022: library + artifact tables (ADR 0002 / ADR 0003).
--
-- PlanetScale Postgres via Hyperdrive. All access uses parameterized
-- statements (never string-interpolated). First consumers are the ingest
-- `opus-64` publication statements in workers/ingest/src/transcode.rs:
-- cache_objects row + per-user track_media row (format='opus',
-- bitrate=64) + user_cache_grants(user, artifact_hash) with the fetching
-- provider_id.
--
-- Numbered 0022 (not 0021): main took 0021 for the billing schema while
-- this slice was in flight, and the gate requires new numbers to continue
-- base_max + 1 (migrations/README.md, check_drift).
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
--   ingest_jobs.id / accepted_at likewise carry defaults
--   (gen_random_uuid()::TEXT, epoch now()) so the billing read path's
--   default-reliant inserts keep working; pipeline writers bind both
--   columns explicitly.
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
--
-- ingest_jobs reconciliation (converge, do not fork): 0021 creates
-- ingest_jobs first for the billing read path as (UUID id, timestamptz
-- accepted_at, CASCADE user FK). The same table is this slice's pipeline
-- job ledger, and track_sources / track_media FK to it as TEXT, so a
-- second CREATE would silently no-op (IF NOT EXISTS) and the FKs would
-- then fail on the type mismatch. Instead this file converges the 0021
-- table to the union shape both slices need: TEXT id, BIGINT epoch
-- accepted_at, RESTRICT user FK, plus the pipeline CHECKs. The billing
-- contract is preserved: its regression test only needs the table to
-- exist, id/accepted_at to have defaults, UNIQUE (user_id,
-- idempotency_key), and the status CHECK, and no Rust code binds the
-- table's columns (verified 2026-10-22: workers/api/src/ledger.rs and
-- plan_limits.rs touch only the ingest_jobs_day plan cap). The USING
-- conversions are total (uuid::text always succeeds and matches the id
-- alphabet; timestamptz epoch extraction always succeeds), so they are
-- safe on any rows 0021 may already hold on staging; every guarded step
-- re-applies as a no-op (0010_metered_admission_claim.sql precedent).

-- Fresh-install path: if 0021 never created the table, build the final
-- shape directly. Post-0021 this no-ops and the DO blocks below converge
-- the billing shape instead. Inline constraints take the deterministic
-- auto-names (ingest_jobs_id_check, ingest_jobs_user_id_fkey, ...) that
-- the converge path below adds explicitly, so both paths land on the
-- identical catalog state.
CREATE TABLE IF NOT EXISTS ingest_jobs (
    id              TEXT PRIMARY KEY DEFAULT (gen_random_uuid()::TEXT)
                    CHECK (id ~ '^[A-Za-z0-9_-]{1,64}$'),
    user_id         TEXT NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    idempotency_key TEXT NOT NULL CHECK (char_length(idempotency_key) BETWEEN 1 AND 768),
    accepted_at     BIGINT NOT NULL DEFAULT (extract(epoch from now())::bigint)
                    CHECK (accepted_at >= 0 AND accepted_at <= 253402300799),
    status          TEXT NOT NULL CHECK (status IN ('queued', 'running', 'done', 'failed')),
    UNIQUE (user_id, idempotency_key)
);

-- Drop stored defaults before any type change: a uuid/timestamptz
-- default cannot be coerced to the new type implicitly, so ALTER TYPE
-- would fail. DROP DEFAULT is a no-op when no default is set, and the
-- matching SET DEFAULT below re-establishes the union shape.
ALTER TABLE ingest_jobs ALTER COLUMN id DROP DEFAULT;
ALTER TABLE ingest_jobs ALTER COLUMN accepted_at DROP DEFAULT;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'ingest_jobs'
          AND column_name = 'id' AND data_type = 'uuid'
    ) THEN
        ALTER TABLE ingest_jobs
            ALTER COLUMN id SET DATA TYPE TEXT USING (id::TEXT);
    END IF;
END
$$;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'ingest_jobs'
          AND column_name = 'accepted_at'
          AND data_type = 'timestamp with time zone'
    ) THEN
        ALTER TABLE ingest_jobs
            ALTER COLUMN accepted_at SET DATA TYPE BIGINT
            USING (extract(epoch FROM accepted_at)::BIGINT);
    END IF;
END
$$;

ALTER TABLE ingest_jobs ALTER COLUMN id SET DEFAULT (gen_random_uuid()::TEXT);
ALTER TABLE ingest_jobs ALTER COLUMN accepted_at SET DEFAULT (extract(epoch from now())::bigint);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'public.ingest_jobs'::regclass
          AND conname = 'ingest_jobs_id_check'
    ) THEN
        ALTER TABLE ingest_jobs
            ADD CONSTRAINT ingest_jobs_id_check
            CHECK (id ~ '^[A-Za-z0-9_-]{1,64}$');
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'public.ingest_jobs'::regclass
          AND conname = 'ingest_jobs_idempotency_key_check'
    ) THEN
        ALTER TABLE ingest_jobs
            ADD CONSTRAINT ingest_jobs_idempotency_key_check
            CHECK (char_length(idempotency_key) BETWEEN 1 AND 768);
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'public.ingest_jobs'::regclass
          AND conname = 'ingest_jobs_accepted_at_check'
    ) THEN
        ALTER TABLE ingest_jobs
            ADD CONSTRAINT ingest_jobs_accepted_at_check
            CHECK (accepted_at >= 0 AND accepted_at <= 253402300799);
    END IF;
END
$$;

DO $$
BEGIN
    -- 0021 names its inline user FK ingest_jobs_user_id_fkey (the
    -- deterministic auto-name); replace CASCADE with the ADR 0002 rule-3
    -- RESTRICT FK. Billing deletes its rows explicitly through the rule-3
    -- gate before the user row goes (0021 header), so no billing flow
    -- depends on CASCADE, and no billing test asserts it.
    IF EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'public.ingest_jobs'::regclass
          AND conname = 'ingest_jobs_user_id_fkey'
          AND confdeltype = 'c'
    ) THEN
        ALTER TABLE ingest_jobs DROP CONSTRAINT ingest_jobs_user_id_fkey;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'public.ingest_jobs'::regclass
          AND conname = 'ingest_jobs_user_id_fkey'
    ) THEN
        ALTER TABLE ingest_jobs
            ADD CONSTRAINT ingest_jobs_user_id_fkey
            FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE RESTRICT;
    END IF;
END
$$;

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
