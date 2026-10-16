-- NNTune v2 migration 0005: atomic auth budgets and test-connection leases.
--
-- Slot rows are claimed with one conditional INSERT .. ON CONFLICT statement.
-- Expiry bounds each sliding window/lease; application calls prune expired
-- rows while reserving, and the indexes keep those cleanups bounded.

CREATE TABLE IF NOT EXISTS auth_rate_limit_slots (
    scope_key   TEXT NOT NULL,
    action      TEXT NOT NULL,
    slot        INTEGER NOT NULL CHECK (slot >= 0),
    expires_at  BIGINT NOT NULL,
    PRIMARY KEY (scope_key, action, slot)
);
CREATE INDEX IF NOT EXISTS idx_auth_rate_limit_expiry
    ON auth_rate_limit_slots (expires_at);
CREATE INDEX IF NOT EXISTS idx_auth_challenges_expiry
    ON auth_challenges (expires_at);

CREATE TABLE IF NOT EXISTS vault_test_leases (
    user_id     TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    slot        SMALLINT NOT NULL CHECK (slot >= 0),
    token       TEXT NOT NULL,
    expires_at  BIGINT NOT NULL,
    PRIMARY KEY (user_id, slot)
);
CREATE INDEX IF NOT EXISTS idx_vault_test_leases_expiry
    ON vault_test_leases (expires_at);
