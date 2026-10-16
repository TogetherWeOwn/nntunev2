-- NNTune v2 migration 0006: fence lease owners across admission retries.
--
-- A lease expiry permits a new request to claim a stalled row, but does not
-- stop the old isolate from resuming. The monotonically increasing epoch lets
-- commit and release updates verify that their caller still owns the claim.
--
-- Apply out of band (staging first) before deploying the worker build; until
-- then admission fails closed (503), never silently.

ALTER TABLE IF EXISTS metered_admission
    ADD COLUMN IF NOT EXISTS claim_epoch BIGINT NOT NULL DEFAULT 0;
