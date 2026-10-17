-- NNTune v2 migration 0009: lease-based metered admission claim.
--
-- Replaces the earlier session advisory-lock claim, which Hyperdrive cannot
-- honor: Hyperdrive pools in transaction mode (one origin connection per
-- transaction), so a `pg_try_advisory_lock` held across queue/meter awaits
-- either never serializes concurrent isolates or fails every claim to 503.
--
-- New claim is a single-statement atomic UPDATE with no session state held
-- across awaits (see `admission_claim_sql` in workers/ingest/src/lib.rs):
--   UPDATE ... SET state='claimed', claimed_at=now()
--   WHERE (state='pending' OR (state='claimed' AND expired))
--   RETURNING ...
-- Postgres serializes concurrent UPDATEs on the same row (row lock +
-- predicate re-check), so exactly one claimant wins; every statement is
-- autocommit, Hyperdrive-safe. `claimed` leases expire after 30 seconds so
-- a lost isolate unblocks retries; `queued` stays terminal (no TTL/delete)
-- until persisted job lifecycle can distinguish retries from terminal jobs.
--
-- Apply out of band (staging first) before the worker build with the new
-- claim rolls out; until then admission fails closed (503), never silently.

ALTER TABLE IF EXISTS metered_admission
    ADD COLUMN IF NOT EXISTS claimed_at TIMESTAMPTZ;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'metered_admission_state_check'
    ) THEN
        ALTER TABLE metered_admission
            ADD CONSTRAINT metered_admission_state_check
            CHECK (state IN ('pending', 'claimed', 'queued'));
    END IF;
END
$$;
