-- Playback lease continuation deadline: an immutable per-play bound on
-- how long a play may keep extending its lease. Existing rows default to
-- 0 (already past), so they fail closed without invented start times;
-- every new acquire overwrites the value.
SET search_path = public, pg_catalog;

ALTER TABLE public.playback_leases
    ADD COLUMN IF NOT EXISTS continuation_until BIGINT NOT NULL DEFAULT 0
    CHECK (continuation_until >= 0);
