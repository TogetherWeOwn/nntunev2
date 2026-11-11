-- NNTune v2 migration 0037: durable per-account collection saves
-- (saved albums + followed artists).
--
-- PlanetScale Postgres via Hyperdrive. All access uses parameterized
-- statements (never string-interpolated): the collection slice owns
-- `workers/api/src/collection.rs` and binds through the API Worker.
--
-- One row per (account, kind, target MBID): `kind = 'album'` is a saved
-- release MBID, `kind = 'artist'` a followed artist MBID. Identity is the
-- composite primary key, which makes save add/remove naturally
-- idempotent: the save statement's `ON CONFLICT` replays the original
-- `saved_at` (the first save instant survives replay and is never
-- refreshed), and removal deletes 0..1 rows.
--
-- Conventions (each pinned to a precedent, not invented here):
-- - `mbid` is TEXT with no format CHECK, like `library.release_mbid`
--   (0035): MBIDs are validated in Rust before binding
--   (`is_catalogue_mbid` in workers/api/src/catalogue.rs) and every
--   read/write keeps the stored value as text — no `::uuid` cast ever
--   appears on the app side.
-- - `saved_at` is BIGINT unix seconds (the `created_at` convention of
--   user-scoped tables), `>= 0`. The `saved_at`-first secondary index
--   backs the bounded `saved_at DESC, mbid` listing.
-- - `user_id` FK is CASCADE, matching the preference-row precedent
--   (`listenbrainz_links`/`listen_events` style): account deletion and
--   the unverified-user reap remove every save row. Writers carry the
--   ADR 0002 rule-3 `writer_fence` in-statement, so no save lands after
--   `users.deleting_at` is set.
-- - No GRANTs (0022 precedent: staging/prod rely on database default
--   privileges; role names are host-owned, docs/ops.md).
-- - Replay-safe: every statement is `IF NOT EXISTS`.
--
-- Conventions (migrations/README.md): the first executable statement is
-- the `SET search_path` header so the migration is self-contained under
-- any role or runner `search_path`, and every relation reference is
-- schema-qualified (`public.…`).

SET search_path = public, pg_catalog;

CREATE TABLE IF NOT EXISTS public.collection_saves (
    user_id  TEXT NOT NULL REFERENCES public.users (id) ON DELETE CASCADE,
    kind     TEXT NOT NULL CHECK (kind IN ('album', 'artist')),
    mbid     TEXT NOT NULL,
    saved_at BIGINT NOT NULL CHECK (saved_at >= 0),
    PRIMARY KEY (user_id, kind, mbid)
);
CREATE INDEX IF NOT EXISTS idx_collection_saves_list
    ON public.collection_saves (user_id, kind, saved_at DESC, mbid);
