-- NNTune v2 migration 0012: bound queued admission replay suppression.
--
-- Queue messages have a maximum retention of 14 days. Preserve existing
-- queued rows for one full maximum retention window, then permit a retry to
-- enqueue the deterministic job again instead of acknowledging a lost message.
-- Re-admission after this horizon may repeat work and a meter event; the
-- consumer's idempotency key must continue to converge final library writes.
--
-- Apply out of band (staging first) before deploying the worker build; until
-- then admission fails closed (503), never silently.

ALTER TABLE IF EXISTS metered_admission
    ADD COLUMN IF NOT EXISTS queued_at TIMESTAMPTZ;

UPDATE metered_admission
SET queued_at = now()
WHERE state = 'queued' AND queued_at IS NULL;
