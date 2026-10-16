-- NNTune v2 migration 0007: database-enforced challenge purpose.
--
-- 0004 made `purpose` immutable by convention only: handlers pass it at
-- insert and check it before the single-use claim, but nothing in the
-- database stopped a future code path (or a manual UPDATE) from
-- re-purposing a live challenge, e.g. flipping a victim's login challenge
-- to `register` and redeeming it to enroll an attacker's passkey.
--
-- Two guards, both role-independent:
--   * a CHECK limiting purpose to the values the handlers define
--     (`PURPOSE_REGISTER`, `PURPOSE_LOGIN`) plus the empty backfill/default
--     value, which every handler purpose check rejects (fail closed);
--   * a trigger that rejects any change of `purpose` after insert, so even
--     a valid-looking value cannot be swapped on a live row. Updates that
--     do not change it (`consumed = TRUE`, the single-use claim) are
--     untouched.
--
-- Idempotent: safe to re-run.

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'auth_challenges'::regclass
          AND conname = 'auth_challenges_purpose_check'
    ) THEN
        ALTER TABLE auth_challenges
            ADD CONSTRAINT auth_challenges_purpose_check
            CHECK (purpose IN ('', 'register', 'login'));
    END IF;
END
$$;

CREATE OR REPLACE FUNCTION auth_challenges_purpose_immutable()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.purpose IS DISTINCT FROM OLD.purpose THEN
        RAISE EXCEPTION 'auth_challenges.purpose is immutable'
            USING ERRCODE = 'restrict_violation';
    END IF;
    RETURN NEW;
END
$$;

DROP TRIGGER IF EXISTS auth_challenges_purpose_immutable ON auth_challenges;
CREATE TRIGGER auth_challenges_purpose_immutable
    BEFORE UPDATE OF purpose ON auth_challenges
    FOR EACH ROW
    EXECUTE FUNCTION auth_challenges_purpose_immutable();
