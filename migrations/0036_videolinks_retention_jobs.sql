-- App-owned retention leases and latest receipts; never mirror data.
-- Fixed environment keys bound both tables to two rows. This is latest-state
-- evidence, not a history or an operational alert delivery mechanism.
SET search_path = public, pg_catalog;

CREATE TABLE IF NOT EXISTS public.video_retention_jobs (
    environment TEXT PRIMARY KEY CHECK (environment IN ('staging', 'production')),
    version BIGINT NOT NULL CHECK (version > 0),
    completed_version BIGINT NOT NULL DEFAULT 0
        CHECK (completed_version >= 0 AND completed_version <= version),
    scheduled_at BIGINT NOT NULL CHECK (scheduled_at > 0),
    build_sha TEXT NOT NULL CHECK (build_sha ~ '^[0-9a-f]{40}$'),
    started_at BIGINT NOT NULL CHECK (started_at > 0),
    lease_until BIGINT NOT NULL CHECK (lease_until >= 0)
);

CREATE TABLE IF NOT EXISTS public.video_retention_receipts (
    environment TEXT PRIMARY KEY REFERENCES public.video_retention_jobs (environment),
    version BIGINT NOT NULL CHECK (version > 0),
    scheduled_at BIGINT NOT NULL CHECK (scheduled_at > 0),
    build_sha TEXT NOT NULL CHECK (build_sha ~ '^[0-9a-f]{40}$'),
    started_at BIGINT NOT NULL CHECK (started_at > 0),
    finished_at BIGINT NOT NULL CHECK (finished_at >= started_at),
    outcome TEXT NOT NULL CHECK (outcome IN
        ('drained', 'due_backlog', 'expired_backlog', 'cleanup_failed')),
    failure TEXT CHECK (failure IN ('invalid_time', 'invalid_receipt', 'store', 'deadline')),
    report JSONB NOT NULL CHECK (jsonb_typeof(report) = 'object'
                               AND pg_column_size(report) <= 4096),
    CHECK ((outcome = 'cleanup_failed') = (failure IS NOT NULL))
);
