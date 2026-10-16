-- NNTune v2 migration 0004: ceremony purpose for passkey challenges.
--
-- A challenge minted for login must never be redeemable at registration
-- (or vice versa): handlers check this column before the single-use
-- claim, and the column is immutable after insert. Pre-existing rows get
-- the empty purpose and therefore fail every purpose check (fail closed;
-- challenges live at most five minutes anyway).

ALTER TABLE auth_challenges
    ADD COLUMN IF NOT EXISTS purpose TEXT NOT NULL DEFAULT '';
