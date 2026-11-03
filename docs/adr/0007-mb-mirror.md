# ADR 0007: Our Own MusicBrainz Mirror (PlanetScale Postgres + Cloudflare Compute)

- **Status:** Accepted; licence terms (Context, points 3 and 6,
  Consequences) amended 2026-11-03 by
  [0012-compliant-youtube-video-and-brainz-data.md](0012-compliant-youtube-video-and-brainz-data.md)
- **Date:** 2026-10-13

## Context

Library matching needs fast, local MusicBrainz lookups (artist / release /
recording + fuzzy search) without depending on `musicbrainz.org` at request
time (rate limits, availability). Core MusicBrainz data is CC0, but the
hourly Live Data Feed replication packets — and supplementary data such as
tags (including genre associations), ratings and annotations — are
CC BY-NC-SA 3.0, and CAA images are copyrighted by their owners with no
licence granted (point 6, as amended). All relational data lives in PlanetScale
Postgres via Hyperdrive (ADR 0001/0002); interactive traffic stays
serverless; batch runs in Containers.

## Decision

1. **One Postgres database `nntunev2-mb`** holds a curated mirror subset:
   `mb_artist`, `mb_release`, `mb_recording` (+ `mb_import_state` for
   replication bookkeeping). Full 30+ table MusicBrainz schema is out of
   scope; the mirror stores what lookup + search need.
2. **Initial import** runs as a Cloudflare Container batch job (`mbdump`
   dumps → Postgres `COPY`). It is idempotent and resumable by line-range
   chunk (`mb_import_cursor`), with poison rows in `mb_import_dlq`.
3. **Twice-weekly CC0 core dumps** keep the mirror current (plus the
   CC0 canonical dumps on the 1st/15th if useful). Freshness SLO:
   **dump age ≤4 days**, monitored per import. The hourly Live Data
   Feed this point originally specified is dropped: its replication
   packets are CC BY-NC-SA 3.0, and the owner chose the free
   CC0-only path with no MetaBrainz supporter tier, account or token
   ([0012-compliant-youtube-video-and-brainz-data.md](0012-compliant-youtube-video-and-brainz-data.md)
   §5, owner decision 2026-11-03; re-open trigger: real revenue).
4. **Read API** lives in `workers/mb` (Rust `workers-rs`): MBID lookup for
   artist / release / recording plus trigram search, over Hyperdrive, with
   KV / Cache API caching. See `workers/mb/README.md`.
5. **Cover art** is cached in the per-environment R2 art buckets
   `nntunev2-art-staging` / `nntunev2-art-prod` (derived renditions per
   ADR 0003; inventory in `docs/ops.md`); the mirror stores only CAA
   URLs/hashes, never hotlinks at request time without cache. Env-correct
   public URLs are deferred until the R2 bindings and public hosts are
   activated.
6. **Terms:** core data is CC0 (no attribution required, credit given
   anyway). Replication packets and supplementary data (tags including
   genre associations, ratings, annotations) are CC BY-NC-SA 3.0 and
   are **excluded at import**: no supporter tier, account or token is
   held, and the service's private, non-commercial framing is not
   relied on to use NC data (the decision and its open questions are
   in
   [0012-compliant-youtube-video-and-brainz-data.md](0012-compliant-youtube-video-and-brainz-data.md)
   §5). CAA grants no licence: images remain copyrighted by their
   owners, and caching/serving them is a flagged open legal risk
   (§6 of that record), not a per-image licence.

## Consequences

- `migrations/0001_mb_mirror.sql` is the source of truth for the mirror
  schema, including `pg_trgm` indexes for search.
- `workers/mb` documents its lookup + search p95 methodology; numbers land
  in its README once staging measures exist.
- Dump freshness (≤4 days) is a monitored SLO; a missed or stale
  dump pages, it never silently skips.
