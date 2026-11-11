-- Invite-budget receipts on registration challenges.
--
-- A register challenge may carry a receipt naming the invite code hash and
-- the email budget key that were charged when the challenge was issued, so
-- the accounting can be contracted later without re-deriving it. The pair is
-- stored beside the challenge row and written in the same INSERT.
--
-- Additive and null-compatible: existing rows and every login/reauth/legacy
-- insert keep both columns NULL. Both-or-neither, and only a register
-- challenge bound to a user may carry a receipt. Once a row carries a
-- receipt it is immutable together with its challenge binding (id, user,
-- purpose, rp_id, origin, expires_at); a receipt can never be attached to an
-- existing row. Consuming the challenge stays allowed. Deleting and
-- re-inserting a reaped id is a fresh INSERT, outside these guards.
--
-- Forward only and replay-safe.
SET search_path = public, pg_catalog;

ALTER TABLE public.auth_challenges ADD COLUMN IF NOT EXISTS invite_budget_code_hash TEXT;
ALTER TABLE public.auth_challenges ADD COLUMN IF NOT EXISTS invite_budget_email_key TEXT;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_catalog.pg_constraint
        WHERE conrelid = 'public.auth_challenges'::regclass
          AND conname = 'auth_challenges_invite_budget_receipt_check'
    ) THEN
        ALTER TABLE public.auth_challenges
            ADD CONSTRAINT auth_challenges_invite_budget_receipt_check
            CHECK (
                (invite_budget_code_hash IS NULL AND invite_budget_email_key IS NULL)
                OR (
                    invite_budget_code_hash IS NOT NULL
                    AND invite_budget_email_key IS NOT NULL
                    AND purpose = 'register'
                    AND user_id IS NOT NULL
                )
            );
    END IF;
END
$$;

CREATE OR REPLACE FUNCTION public.auth_challenges_invite_budget_receipt_guard()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = public, pg_catalog
AS $$
BEGIN
    IF OLD.invite_budget_code_hash IS NULL AND OLD.invite_budget_email_key IS NULL THEN
        IF NEW.invite_budget_code_hash IS NOT NULL OR NEW.invite_budget_email_key IS NOT NULL THEN
            RAISE EXCEPTION 'auth_challenges invite budget receipt cannot be attached to an existing row'
                USING ERRCODE = 'restrict_violation';
        END IF;
        RETURN NEW;
    END IF;
    IF NEW.invite_budget_code_hash IS DISTINCT FROM OLD.invite_budget_code_hash
       OR NEW.invite_budget_email_key IS DISTINCT FROM OLD.invite_budget_email_key
       OR NEW.id IS DISTINCT FROM OLD.id
       OR NEW.user_id IS DISTINCT FROM OLD.user_id
       OR NEW.purpose IS DISTINCT FROM OLD.purpose
       OR NEW.rp_id IS DISTINCT FROM OLD.rp_id
       OR NEW.origin IS DISTINCT FROM OLD.origin
       OR NEW.expires_at IS DISTINCT FROM OLD.expires_at THEN
        RAISE EXCEPTION 'auth_challenges invite budget receipt and challenge binding are immutable'
            USING ERRCODE = 'restrict_violation';
    END IF;
    RETURN NEW;
END
$$;

DROP TRIGGER IF EXISTS auth_challenges_invite_budget_receipt_guard ON public.auth_challenges;
CREATE TRIGGER auth_challenges_invite_budget_receipt_guard
    BEFORE UPDATE ON public.auth_challenges
    FOR EACH ROW
    EXECUTE FUNCTION public.auth_challenges_invite_budget_receipt_guard();
