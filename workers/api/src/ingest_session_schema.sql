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
    terminal_reason TEXT CHECK (terminal_reason IN ('aborted', 'expired', 'completed')),
    terminal_at TIMESTAMPTZ,
    CHECK ((terminal_reason IS NULL) = (terminal_at IS NULL)),
    completed_map_id UUID REFERENCES public.storage_digest_inventory_intents (map_id),
    CHECK ((terminal_reason IS NOT DISTINCT FROM 'completed') = (completed_map_id IS NOT NULL)),
    -- Only the checkpoint coordinates; no validator or sealer state lives here.
    checkpoint_generation UUID,
    checkpoint_revision BIGINT NOT NULL DEFAULT 0,
    checkpoint_offset BIGINT NOT NULL DEFAULT 0 CHECK (checkpoint_offset BETWEEN 0 AND 8589934592),
    CHECK (checkpoint_revision BETWEEN 0 AND checkpoint_offset),
    CHECK ((checkpoint_revision = 0) = (checkpoint_offset = 0)),
    CHECK ((checkpoint_revision = 0) = (checkpoint_generation IS NULL)),
    -- A single writer at the current checkpoint revision and session deadlines.
    write_lease_id UUID,
    write_lease_generation UUID,
    CHECK ((write_lease_id IS NULL) = (write_lease_generation IS NULL)),
    CHECK (terminal_reason IS NULL OR write_lease_id IS NULL),
    CHECK (checkpoint_generation IS NULL OR write_lease_generation IS NULL
           OR write_lease_generation = checkpoint_generation),
    -- The library identity is globally unique and its environment is immutable.
    UNIQUE (library_id, principal_id, idempotency_key)
);

-- Durable intent and queue handoff state, never a deletion receipt.
-- Keep the intent identity and due time unchanged across dispatch attempts.
CREATE TABLE public.ingest_session_cleanup_intents (
    session_id UUID NOT NULL REFERENCES public.ingest_session_reservations (id),
    kind TEXT NOT NULL CHECK (kind IN ('absolute_expiry', 'terminal_reconcile')),
    due_at TIMESTAMPTZ NOT NULL,
    delivery_claim_id UUID,
    claimed_until TIMESTAMPTZ,
    enqueued_at TIMESTAMPTZ,
    CHECK ((delivery_claim_id IS NULL) = (claimed_until IS NULL)),
    CHECK (enqueued_at IS NULL OR delivery_claim_id IS NULL),
    PRIMARY KEY (session_id, kind)
);
