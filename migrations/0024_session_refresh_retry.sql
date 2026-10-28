-- Optional client-bound refresh recovery. One bounded receipt per session:
-- only a SHA-256 digest of the predecessor cookie + 256-bit retry secret and
-- the successor issuance time. No raw token/secret or growing receipt log.
-- Existing rows and empty-body refreshes retain strict replay rejection.
ALTER TABLE sessions ADD COLUMN IF NOT EXISTS refresh_retry_hash TEXT;
ALTER TABLE sessions ADD COLUMN IF NOT EXISTS refresh_issued_at BIGINT;
