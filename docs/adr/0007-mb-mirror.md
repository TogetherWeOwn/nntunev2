# ADR 0007: Our Own MusicBrainz Mirror (PlanetScale Postgres + Cloudflare Compute)

- **Status:** Accepted; licence terms (Context, points 3 and 6,
  Consequences) amended 2026-11-03 by
  [0012-compliant-youtube-video-and-brainz-data.md](0012-compliant-youtube-video-and-brainz-data.md)
- **Date:** 2026-10-13

## 2026-11-07 amendment: retirement and conditional bounded replacement

This amendment supersedes conflicting Context, Decision and Consequences text
below; unchanged licence and cover-art restrictions remain binding.

The former `nntunev2-mb` cluster was retired on 2026-11-06 to stop unused
recurring cost; its seed was not a completed import. Dependent Hyperdrive
configuration and the repository MB variable were removed. A still-deployed
Worker is not an available mirror. App databases and production were unchanged.

The R2-snapshot experiment subsequently failed its interactive latency criterion.
The owner permitted a bounded replacement: **PS-5, at most $10/month all-in,
curated CC0 10-table subset, one main branch, no development branch or extra
replicas**. Reported setup on 2026-11-07 is not a completed import or activated
read service. Larger allocation requires measured PS-5 failure and a new spend
decision. See [the dated operations inventory](../ops.md#planetscale-postgres).

This replaces the Container-based initial-import decision: initial load is one
manual trusted-host ETL → validated COPY operation, with TLS, least privilege,
native validator source/digest/architecture/ABI provenance, security review and
exact-source release acceptance. No Containers, unvalidated COPY or host cron
is authorized. Synthetic-fixture-only reseeding is not a substitute for the
coordinated full-load handoff; old grants and migration receipts do not prove
replacement readiness.

Weekly refreshes use CC0 full exports; increase to twice-weekly only for
demonstrated product need. Weekly automation remains disabled until its
non-Container runtime and per-table streaming/headroom requirements are
resolved. The old Container cost estimate is not approval of that runtime.
The former ≤4-day freshness figure is historical, not a weekly guarantee or
monitored SLO.

Read scope remains `/v1/mb/*`, not full WS/2 parity. Planned cache measures are
MBID lookups/listings with dump-id keys and long TTLs, invalidation on dump-pointer
change, query caching only on a newly verified MB config, and placement near the
database. Record uncached/cached p95 and cache-hit ratio; this amendment does
not claim those measures implemented. Track the mirror's first actual invoice
independently of the app database invoice against the all-in ceiling.

Keep CC0-only and cover-art rights restrictions, security and independent
review, exact-head Actions, tracked binding inventory and release gates.
No restoration of retired identifiers, automatic activation, additional
provisioning or production change is authorized by this record.

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
   CC0 canonical dumps on the 1st/15th if useful). Freshness target:
   **dump age ≤4 days**. The hourly Live Data
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
- Dump freshness (≤4 days) is a target, not a live monitored SLO:
  no dump-age monitor or pager exists yet. When they are built, a
  missed or stale dump must page — never silently skip. Until then
  nothing here claims alerting coverage.
