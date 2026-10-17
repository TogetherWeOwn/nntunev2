-- NNTune v2 migration 0013: allow session-bound reauthentication challenges.
-- Migration 0008 installed the purpose allowlist before the reauth route was
-- added. Replace the constraint without weakening immutability of live rows.
-- Idempotent: safe to re-run.

ALTER TABLE auth_challenges
    DROP CONSTRAINT IF EXISTS auth_challenges_purpose_check;

ALTER TABLE auth_challenges
    ADD CONSTRAINT auth_challenges_purpose_check
    CHECK (purpose IN ('', 'register', 'login', 'reauth'));
