-- Draft storage contract, not in the migration manifest or mounted in the API.
-- A reservation alone does not authorize sealing, provider writes or publication.
CREATE TABLE public.ingest_session_reservations (
    id UUID PRIMARY KEY,
    library_id UUID NOT NULL REFERENCES public.storage_library_namespaces (id),
    principal_id TEXT NOT NULL REFERENCES public.users (id),
    idempotency_key TEXT COLLATE "C" NOT NULL CHECK (idempotency_key ~ '^[!-~]{1,128}$'),
    fingerprint BYTEA NOT NULL CHECK (octet_length(fingerprint) = 32),
    absolute_deadline TIMESTAMPTZ NOT NULL,
    idle_deadline TIMESTAMPTZ NOT NULL CHECK (idle_deadline <= absolute_deadline),
    -- A permanent fence: checkpoint and publication writers must lock this row
    -- and require a null terminal marker before advancing.
    terminal_reason TEXT CHECK (terminal_reason IN ('aborted', 'expired')),
    terminal_at TIMESTAMPTZ,
    CHECK ((terminal_reason IS NULL) = (terminal_at IS NULL)),
    -- Only the checkpoint coordinates; no validator or sealer state lives here.
    checkpoint_generation UUID,
    checkpoint_revision BIGINT NOT NULL DEFAULT 0,
    checkpoint_offset BIGINT NOT NULL DEFAULT 0 CHECK (checkpoint_offset BETWEEN 0 AND 8589934592),
    CHECK (checkpoint_revision BETWEEN 0 AND checkpoint_offset),
    CHECK ((checkpoint_revision = 0) = (checkpoint_offset = 0)),
    CHECK ((checkpoint_revision = 0) = (checkpoint_generation IS NULL)),
    -- The library identity is globally unique and its environment is immutable.
    UNIQUE (library_id, principal_id, idempotency_key)
);
