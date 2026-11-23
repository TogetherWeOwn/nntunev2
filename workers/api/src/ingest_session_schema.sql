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
    -- The library identity is globally unique and its environment is immutable.
    UNIQUE (library_id, principal_id, idempotency_key)
);
