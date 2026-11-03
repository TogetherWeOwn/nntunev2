-- 0031_videolinks_curation: app-side video-link curation tables.
--
-- N-videolinks pipeline storage (harvest -> validate -> TTL -> coverage).
-- App-side on purpose: the mb_* mirror stays CC0-only (MusicBrainz core
-- data), while these tables hold ListenBrainz-observed links, moderated
-- user submissions and YouTube Data API-returned metadata — none of
-- which may be ingested into the product mirror.
--
-- YouTube rules honoured here by shape (AGENTS.md guardrails):
-- - Only video IDs are stored long-term. Every other API-returned field
--   (title, channel) carries validated_at/expires_at with a 30-day TTL
--   and must be refreshed or deleted past expiry.
-- - Thumbnails are never proxied, cached or stored: there is no
--   thumbnail column anywhere in this migration.
-- - No search state: harvest sources are curated links only, never
--   `search.list` output.
--
-- Replay-safety: every statement is IF NOT EXISTS / additive; rerunning
-- this file is a clean no-op.

-- Harvested + validated candidates: one row per (recording, video).
-- recording_mbid intentionally carries no FK: the app database does not
-- own the mirror's mb_recording table, and curation must survive mirror
-- re-imports.
CREATE TABLE IF NOT EXISTS video_link_candidates (
    recording_mbid UUID NOT NULL,
    video_id       TEXT NOT NULL CHECK (video_id ~ '^[A-Za-z0-9_-]{11}$'),
    source         TEXT NOT NULL
        CHECK (source IN ('mb_url', 'wikidata', 'listenbrainz', 'user_submission')),
    status         TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'validated', 'rejected', 'expired')),
    -- API-returned display metadata (TTL'd, see expires_at). Never thumbnails.
    title          TEXT,
    channel_id     TEXT,
    duration_secs  INTEGER CHECK (duration_secs IS NULL OR duration_secs >= 0),
    embeddable     BOOLEAN,
    -- Unix epoch seconds of the last successful videos.list validation;
    -- 0 (never validated) rows are not servable.
    validated_at   BIGINT NOT NULL DEFAULT 0,
    -- validated_at + 30 days. Writers compute this; readers treat
    -- expires_at <= now() as stale (refresh or delete, never serve).
    expires_at     BIGINT NOT NULL DEFAULT 0,
    first_seen_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (recording_mbid, video_id)
);

-- Servable-link lookup: a recording's fresh validated links.
CREATE INDEX IF NOT EXISTS ix_video_link_candidates_recording
    ON video_link_candidates (recording_mbid);

-- Revalidation sweep: stale rows past their TTL.
CREATE INDEX IF NOT EXISTS ix_video_link_candidates_expires
    ON video_link_candidates (expires_at);

-- Moderation queue for user submissions. Approved rows are harvested as
-- user_submission candidates; rejected rows stay as an audit trail.
CREATE TABLE IF NOT EXISTS video_user_submissions (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    recording_mbid UUID NOT NULL,
    video_id       TEXT NOT NULL CHECK (video_id ~ '^[A-Za-z0-9_-]{11}$'),
    submitted_by   TEXT NOT NULL,
    status         TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'rejected')),
    reason         TEXT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    decided_at     TIMESTAMPTZ
);

-- Moderator worklist: pending submissions, oldest first.
CREATE INDEX IF NOT EXISTS ix_video_user_submissions_pending
    ON video_user_submissions (created_at)
    WHERE status = 'pending';

-- Coverage snapshots: total + played-weighted ratios as computed by the
-- pipeline (see workers/mb/src/videolinks.rs). played_coverage_ratio is
-- NULL until N-plays data is available.
CREATE TABLE IF NOT EXISTS video_coverage_snapshots (
    computed_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    total_recordings      BIGINT NOT NULL CHECK (total_recordings >= 0),
    linked_recordings     BIGINT NOT NULL CHECK (linked_recordings >= 0),
    coverage_ratio        DOUBLE PRECISION NOT NULL
        CHECK (coverage_ratio >= 0 AND coverage_ratio <= 1),
    played_coverage_ratio DOUBLE PRECISION
        CHECK (played_coverage_ratio IS NULL
               OR (played_coverage_ratio >= 0 AND played_coverage_ratio <= 1)),
    PRIMARY KEY (computed_at)
);
