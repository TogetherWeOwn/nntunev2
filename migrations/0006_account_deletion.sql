-- NNTune v2 migration 0006: account deletion scheduling and durable operations.
--
-- Forward-only additive change. Account operations are retained auth-domain
-- references and therefore restrict user deletion until the later executor
-- removes them in the final account-erasure transaction (ADR 0002 rule 3).

ALTER TABLE users
    ADD COLUMN deleting_at TIMESTAMPTZ NULL;

CREATE TABLE delete_operations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    scope TEXT NOT NULL CHECK (scope IN ('account', 'library', 'playlist')),
    container_id TEXT NULL,
    cursor JSONB NOT NULL DEFAULT '{}'::jsonb,
    state TEXT NOT NULL DEFAULT 'pending'
        CHECK (state IN ('scheduled', 'pending', 'running', 'failed', 'done')),
    erase_after TIMESTAMPTZ NULL
        CHECK ((scope = 'account') = (erase_after IS NOT NULL)),
    heartbeat_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    consecutive_failures SMALLINT NOT NULL DEFAULT 0
        CHECK (consecutive_failures >= 0),
    last_error TEXT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    CHECK ((scope = 'account') = (container_id IS NULL))
);

CREATE UNIQUE INDEX delete_operations_one_account_per_user
    ON delete_operations (user_id)
    WHERE scope = 'account';

CREATE UNIQUE INDEX delete_operations_one_container_operation
    ON delete_operations (user_id, scope, container_id)
    WHERE scope IN ('library', 'playlist');
