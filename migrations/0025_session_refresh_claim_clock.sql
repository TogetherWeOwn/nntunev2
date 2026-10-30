-- Keep PostgreSQL receipt age separate from signed-token issuance. Tokens
-- retain the already-validated Worker issuance time; the DB clock only bounds
-- serialized recovery. Existing receipts without this clock fail closed.
ALTER TABLE sessions ADD COLUMN IF NOT EXISTS refresh_claimed_at BIGINT;
