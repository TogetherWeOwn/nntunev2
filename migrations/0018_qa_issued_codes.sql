-- NNTune v2 migration 0018: staging-only emailed-code retrieval outbox.
-- Holds the RAW issued magic token per synthetic mailbox so the staging-only
-- POST /api/test-codes seam can return the issued code without minting an
-- account. This is a deliberate, narrowly-scoped exception to the migration
-- 0002 invariant ("the plaintext token is emailed once and never stored"):
-- rows are written ONLY when the QA code-seam flag is on AND the recipient
-- is an `example.test` mailbox (never routable, never a real user), and each
-- mailbox keeps a single latest-token row that expires with the magic-link
-- TTL. Production never sets the flag, so this table stays empty there.
-- Idempotent: safe to re-run.
CREATE TABLE IF NOT EXISTS qa_issued_codes (
    email       TEXT NOT NULL PRIMARY KEY,
    token       TEXT NOT NULL,
    issued_at   BIGINT NOT NULL
);
