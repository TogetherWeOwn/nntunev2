-- NNTune v2 migration 0035: ingest `library` table (M1 ingest→library write path).
--
-- PlanetScale Postgres via Hyperdrive. All access uses parameterized
-- statements (never string-interpolated): the ingest write owns
-- `workers/ingest/src/library.rs::upsert_library_sql`, the catalogue reads
-- own `workers/api/src/catalogue.rs`.
--
-- Enablement: until this migration the ingest consumer's library upsert
-- fails deterministically on the missing table (see `crate::library` in
-- workers/ingest) and no `/v1/library` (or `/api/library` alias) row can
-- exist. This table is the per-user flat ingest ledger the M1
-- ingest→play→meter journey reads: one row per ingest job carrying the
-- matched release MBID plus the release's recording MBIDs in album order.
-- Rich metadata (titles, durations, positions, artist) is NOT duplicated
-- here: it stays authoritative in the MusicBrainz mirror (`mb_artist`,
-- `mb_release`, `mb_recording`, `mb_track` from migrations 0001/0016) and
-- the catalogue reads join to it. Unknown MB metadata stays NULL on read;
-- it is never filled from tenant file tags (ADR 0002 rule 6).
--
-- Identity: `idempotency_key` is the row identity (the ingest queue's
-- redelivery key) and the upsert's `ON CONFLICT` target. `job_id` is the
-- pipeline job id verbatim (TEXT, the jobs::validate_job_id alphabet,
-- like 0022 `ingest_jobs.id`), carried as a regular column so a refined
-- match replay can converge it via `DO UPDATE`.
--
-- Conventions (each pinned to a precedent, not invented here):
-- - `user_id` FK is RESTRICT (ADR 0002 rule 3, 0022 precedent): library
--   deletion is an explicit fenced batched procedure, never a bare
--   cascade. The upsert carries the in-statement `writer_fence` on the
--   owner row, so no write lands after `users.deleting_at` is set.
-- - MBIDs are TEXT, NULL-able for unmatched ingests, with no format
--   CHECK: the ingest matcher validates MBIDs in Rust before binding
--   (`validate_mbid` in workers/ingest/src/queue.rs) and the catalogue
--   reads guard every mirror join with the same shape, so a malformed
--   stored value degrades one row's enrichment, never the listing.
-- - `recording_mbids` is `TEXT[] NOT NULL DEFAULT '{}'`: an unmatched
--   ingest writes the empty array by design (see
--   `recording_mbids_or_fallback` in workers/ingest/src/matching.rs).
-- - No GRANTs (0022 precedent: staging/prod rely on database default
--   privileges; role names are host-owned, docs/ops.md).
-- - Replay-safe: every statement is `IF NOT EXISTS`.
--
-- Conventions (migrations/README.md): the first executable statement is the
-- `SET search_path` header so the migration is self-contained under any
-- role or runner `search_path`, and every relation reference is
-- schema-qualified (`public.…`).

SET search_path = public, pg_catalog;

CREATE TABLE IF NOT EXISTS public.library (
    idempotency_key TEXT PRIMARY KEY,
    job_id          TEXT NOT NULL,
    user_id         TEXT NOT NULL REFERENCES public.users (id) ON DELETE RESTRICT,
    release_mbid    TEXT NULL,
    recording_mbids TEXT[] NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_library_user ON public.library (user_id);
