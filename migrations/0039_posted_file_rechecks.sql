-- Durable bounded recheck scheduling for the existing generation/round fence.
-- No worker is enabled, legacy custody changed, or replacement/cleanup granted.
-- A stopped schedule is NOT conclusive negative health. Existing rounds without
-- durable scheduling history are untracked, never assigned invented counters or
-- reset to a fresh budget. They require an explicit new verification round.

SET search_path = public, pg_catalog;

CREATE TABLE IF NOT EXISTS posted_file_recheck_schedule (
    generation_id TEXT NOT NULL REFERENCES posted_file_health(generation_id) ON DELETE RESTRICT,
    round BIGINT NOT NULL CHECK (round >= 1),
    completed_passes INTEGER NOT NULL DEFAULT 0 CHECK (completed_passes BETWEEN 0 AND 5),
    state TEXT NOT NULL CHECK (state IN ('waiting', 'exhausted', 'untracked')),
    not_before BIGINT CHECK (not_before BETWEEN 0 AND 253402300799),
    PRIMARY KEY (generation_id, round),
    CHECK ((state = 'waiting' AND completed_passes < 5 AND not_before IS NOT NULL)
        OR (state = 'exhausted' AND completed_passes = 5 AND not_before IS NULL)
        OR (state = 'untracked' AND completed_passes = 0 AND not_before IS NULL))
);

-- Replay never resets a managed schedule or invents pass history. Old evidence,
-- same-ID repost accounting, round identity and all custody remain untouched.
INSERT INTO posted_file_recheck_schedule (generation_id, round, state)
    SELECT generation_id, round, 'untracked' FROM posted_file_health WHERE round > 0
    ON CONFLICT (generation_id, round) DO NOTHING;
