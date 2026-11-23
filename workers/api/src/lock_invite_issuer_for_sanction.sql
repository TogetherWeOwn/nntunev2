-- First statement of the unmounted sanction transaction (READ COMMITTED).
-- $1: issuer ID. No row means missing or already restricted. Hold the lock
-- through commit; registration can still take KEY SHARE for its tree FK.
-- Before mounting: admin authorization under the admin-write advisory lock
-- must not take FOR UPDATE on a user that in-flight enrollment can reference.
-- Member issuance and both admin issuance paths must check the marker under
-- the issuer lock. Both claim paths must resolve the current issuer, check the
-- marker under a conflicting row lock before locking the invite, and recheck
-- the invite's issuer on claim (including the NULL/orphan case).
SELECT id FROM public.users
WHERE id = $1 AND invite_issuance_restricted_at IS NULL
FOR NO KEY UPDATE
