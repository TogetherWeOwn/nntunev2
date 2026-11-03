-- NNTune v2 migration 0027: track_sources becomes global (ADR 0013).
--
-- track_sources (migration 0022) was keyed per owner
-- (PRIMARY KEY (user_id, track_id) with a users FK) for the bring-your-own-
-- provider era, where each user's own fetch attested their own provenance
-- slot. Under ADR 0013 there is one shared catalogue ingested by the
-- service through backend provider accounts: provenance is one curated
-- source per track, published globally, so the table converges to
-- PRIMARY KEY (track_id) with no user identity.
--
-- Collapsing duplicate per-track rows is safe: the per-user rows are
-- BYOP-era shape, and the track writers have no executed production caller
-- (the queue consumer path that would execute them is disabled), so staging
-- holds at most test seeds. Where a track somehow holds several rows, the
-- survivor is deterministic: lowest user_id first, then lowest
-- source_job_id, with ctid as the final tiebreak — first provenance wins,
-- matching the writers' ON CONFLICT (track_id) DO NOTHING.
--
-- track_media and user_cache_grants are deliberately untouched here; their
-- globalisation is a separate change with its own migration.
--
-- Converge idiom (migrations/0022_library_artifacts.sql precedent): every
-- step is guarded on the old shape, so a replay is a clean no-op
-- (workers/api/tests/run_migration_tests.sh re-applies every migration).
-- The outer guard keys on the user_id column: once it is gone the whole
-- body skips. Inner guards name the deterministic auto-constraint names
-- (track_sources_pkey, track_sources_user_id_fkey) exactly as 0022's
-- inline definitions created them.

DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'track_sources'
          AND column_name = 'user_id'
    ) THEN
        -- Dedupe BEFORE the PK change: keep exactly one row per track_id.
        DELETE FROM track_sources doomed
        USING (
            SELECT ctid,
                   row_number() OVER (
                       PARTITION BY track_id
                       ORDER BY user_id ASC, source_job_id ASC, ctid
                   ) AS rn
            FROM track_sources
        ) ranked
        WHERE doomed.ctid = ranked.ctid AND ranked.rn > 1;

        -- Drop the old per-owner PK and the users FK it leans on.
        IF EXISTS (
            SELECT 1 FROM pg_constraint
            WHERE conrelid = 'public.track_sources'::regclass
              AND conname = 'track_sources_pkey'
        ) THEN
            ALTER TABLE track_sources DROP CONSTRAINT track_sources_pkey;
        END IF;
        IF EXISTS (
            SELECT 1 FROM pg_constraint
            WHERE conrelid = 'public.track_sources'::regclass
              AND conname = 'track_sources_user_id_fkey'
        ) THEN
            ALTER TABLE track_sources DROP CONSTRAINT track_sources_user_id_fkey;
        END IF;

        -- Drop the owner column; the RESTRICT FKs to tracks/ingest_jobs and
        -- every envelope column stay exactly as 0022 shaped them.
        ALTER TABLE track_sources DROP COLUMN user_id;

        -- One global provenance slot per track.
        IF NOT EXISTS (
            SELECT 1 FROM pg_constraint
            WHERE conrelid = 'public.track_sources'::regclass
              AND conname = 'track_sources_pkey'
        ) THEN
            ALTER TABLE track_sources
                ADD CONSTRAINT track_sources_pkey PRIMARY KEY (track_id);
        END IF;
    END IF;
END
$$;

-- The per-track lookup index is redundant under PRIMARY KEY (track_id);
-- the job lookup index stays (sweep/GC paths reach sources by job).
DROP INDEX IF EXISTS idx_track_sources_track;
CREATE INDEX IF NOT EXISTS idx_track_sources_job ON track_sources (source_job_id);
