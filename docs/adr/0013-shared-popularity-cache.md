# ADR 0013: Shared popularity cache (supersedes the per-user grant layer of ADR 0003)

- **Status:** Superseded by [ADR 0015](0015-standard-s3-storage-boundary.md)
  (2026-11-13); retained as historical design, not new cache-build authority.
- **Date:** 2026-11-03
- **Supersedes:** the per-user authorization and accounting layer of
  ADR 0003 (`user_cache_grants`, own-provider byte proof, per-user cache
  budgets/retention/priority) and the cache columns of the ADR 0004 plan
  matrix. ADR 0003's shared content-addressed storage, R2 bucket map and
  publication/sweep race rules are retained and restated where they change.

- **Amended by:** [ADR 0015](0015-standard-s3-storage-boundary.md) (approved
  target, 2026-11-13): the provider is to own caching, so the NNTP miss path
  and cache fills here are targeted for replacement by a read-only S3 GET.
  This record stays as history of the merged design; nothing is migrated yet.

## Context

ADR 0003 designed for bring-your-own-provider (BYOP): every user fetched
audio through their own Usenet account, so a global `cache_objects` row
could never authorize a serve. Serving required a `user_cache_grants` row
attesting that *that user's own provider* had fetched and hash-verified the
bytes, plus a per-user byte budget. That machinery existed to stop one
user's fetched bytes (or bad match) from being redistributed to everyone
else.

The product model has changed, and with it the reason for grants:

- Users no longer bring providers or NZBs. There is one shared, curated
  catalogue, ingested by an admin and fetched through backend provider
  accounts (ADR 0014). Every byte in the cache was fetched by the service
  itself from a curated source, so "whose provider proved these bytes" is
  always the service.
- What a play needs now is an entitlement check (invited, subscribed)
  against a track in the shared catalogue — not a per-user byte proof.
  Per-user cache budgets, retention tiers and cache priority were plan
  differentiators for a storage allowance that no longer exists.

What the cache is *for* also changes. Under BYOP it saved a user's own
provider quota. Now it protects a small pool of shared backend accounts:
popular tracks should be served from R2 so repeated plays across all
users do not re-fetch the same articles over NNTP. That makes admission a
popularity decision, driven by real play activity, under one global
storage cap — the cache is a cost control, and the cap is the spend bound.

Implementation state today: the `cache_objects` / `track_media` /
`user_cache_grants` schema exists (migration 0022), ingest has grant
writers, but no stream reader checks grants, there is no cache-GC
Workflow, no plays write path yet (the `/v1/plays` surface is specified
but unbuilt; `stream_play_ledger` exists with no production writer), and
the stream crate's in-memory `track/<id>` pointer cache is already global.
This ADR therefore designates target behaviour; §Consequences lists the
alignment work rather than claiming any of it exists.

## Decision

**One shared, content-addressed audio cache, admitted by popularity,
bounded by a single configurable byte cap.** Authorization to serve a
track is: authenticated caller, active entitlement, and the track is
available in the shared catalogue. Presence in the cache is never
authorization and its absence is never a denial — it only chooses between
an R2 read and an NNTP fetch through the provider pool (ADR 0014).

### What is retained from ADR 0003

- Content-addressed storage: `cache_objects` keyed by SHA-256, the
  per-environment buckets `nntunev2-{audio,art}-{staging,prod}` and the
  `audio/{aa}/{bb}/{sha256hex}` / `art/{aa}/{bb}/{sha256hex}` key layout.
  Objects are immutable and hash-verified before publication.
- No hit oracles: `cached_hit` stays server-side telemetry. No hit/miss
  headers, no hash-existence endpoints. A cache lookup result must not be
  observable in any response distinction beyond timing.
- The publication/sweep race rules: per-hash serialization held from R2
  PUT through pointer publication, recheck of reference sets before any
  physical delete, orphan sweep only after 24 hours and a serialized
  recheck, physical-delete failures retried without restoring access.
- The artwork model, with "the caller's library membership" read as
  "catalogue availability plus entitlement": validated JPEG/PNG/WebP,
  size/megapixel bounds, fixed host allowlist for Cover Art Archive,
  negative-result caching, 30-day idle window, authenticated serve with
  private/no-store responses. Embedded art is extracted once, at admin
  ingest, never fetched through a user source (there are none).

### What is removed

- `user_cache_grants` and the own-provider byte-proof rule, including the
  composite `(user_id, provider_id)` FK discipline built around it.
- The per-user `cache_bytes` live gauge and the ADR 0004 columns
  `cache_bytes_budget`, `cache_retention_days`, `cache_priority`. Plan
  enforcement that remains (stream hours, one concurrent stream) is
  untouched by this record.
- Per-user `track_media` semantics. `track_media` becomes a global
  track → content-hash mapping: one curated match per track, published by
  the service. A bad admin match is corrected for everyone at once by
  republishing the mapping; that is the deliberate trade for losing
  per-user poisoning isolation, and it is why ingest keeps its
  match/verify gates.
- Own-provider refetch as the miss path. A miss fetches through the
  backend provider pool from the track's curated source.

### Popularity signal and admission

- The signal is the plays write path: one play event per playback start,
  recorded for every playback source, with skip/complete outcomes. Until
  that path lands, stream-serve events are the interim source; the rollup
  below reads either through one query, so the swap is not a redesign.
- A scheduled rollup (Workflow, hourly) maintains `track_popularity`:
  play counts over trailing 24-hour, 7-day and 30-day windows
  (`plays_24h`, `plays_7d`, `plays_30d`), distinct listener counts over
  **both** trailing windows admission and scoring need
  (`distinct_listeners_7d`, `distinct_listeners_30d`), `last_played_at`,
  and a `score`. Distinct counts are `COUNT(DISTINCT listener)` computed
  by the rollup directly from play events over each trailing window —
  never by summing per-day or per-bucket distinct counts, which would
  double-count a listener active in more than one bucket. Admission
  below reads `distinct_listeners_7d` against
  `popcache.admit_min_listeners_7d`; it never derives a 7-day distinct
  count from the 30-day figure. The score weights recent and
  distinct-listener plays over raw repeats, so one user looping a track
  cannot pin it alone. The exact weights are configuration (below), not
  schema.
- Admission is **write-on-threshold, never write-on-first-play**: a first
  play streams over NNTP and only counts. A track becomes fill-eligible
  when it crosses the configured threshold (initial default: at least 3
  plays by at least 2 distinct listeners in 7 days). An admin pin flag
  can force eligibility (for example a newly ingested featured release);
  pins count against the same cap.
- Fill is asynchronous background work, never on the play request path
  beyond enqueuing: a fill job fetches via the provider pool in its
  lowest-priority budget class (ADR 0014), hash-verifies the complete
  object, then publishes `cache_objects` + `track_media` under the
  per-hash serialization above. Concurrent fills for the same track
  coalesce onto one job. A failed fill records a retry-after and never
  publishes partial bytes.
- The serve path reads R2 when a published mapping exists, else fetches
  over NNTP. A mapping whose R2 object is missing is treated as a miss
  and re-queued for fill; the play still succeeds over NNTP.

### Cap and eviction

- One global cap, `popcache.cap_bytes`, is the hard bound on cached audio
  bytes per environment (art keeps its separate idle window). It is an
  operator-set value; initial defaults are 10 GiB on staging and 100 GiB
  on production. **No admission may exceed the cap**: at the cap, fills
  stop until eviction frees space. There is no per-user share of the cap.
- Eviction is score-ordered, run by the cache-GC Workflow in bounded
  batches: when the live gauge (`SUM(byte_size)` over audio objects
  referenced by `track_media`) crosses the high watermark (95% of cap),
  evict lowest-score mappings — tie-break least-recently-served — down to
  the low watermark (90%). Idle decay is inherent in the trailing-window
  score; a track no one plays falls out on its own.
- Eviction deletes the mapping and, once an object has zero
  `track_media` and zero `release_art` references under the serialized
  recheck, the object. It never deletes the curated source: an evicted
  track re-fills from its NZB through the pool if it becomes popular
  again. Eviction is always safe to run — worst case is a slower next
  play, never data loss.
- An object larger than the whole cap is streamed without admission.

### Configuration

All knobs live in the shared config registry (`crates/nntune-config`
`KeySpec` entries), consistent with existing keys such as
`ingest.max_nzb_bytes`:

| Key | Meaning | Initial default |
|---|---|---|
| `popcache.cap_bytes` | global audio cap per environment | 10 GiB staging / 100 GiB prod |
| `popcache.high_watermark_pct` / `popcache.low_watermark_pct` | eviction triggers | 95 / 90 |
| `popcache.admit_min_plays_7d` | plays in 7 days to be fill-eligible | 3 |
| `popcache.admit_min_listeners_7d` | distinct listeners in 7 days | 2 |
| `popcache.score_weights` | window/distinct weights for `score` | tuned on staging data |

Changing the cap downward is safe at any time: the next GC batch evicts
to the new low watermark. Raising it is the only spend-relevant action,
and it is an operator decision recorded in config history.

### Playback source order (consumption contract)

This record states its own consumption contract, self-contained, so
it can be read without following the cross-reference below. The
playback resolver order implementations of this ADR must follow is: **curated FLAC — R2
popularity cache first, then NNTP through the provider pool — then the
YouTube embed, then unavailable**. Unavailable tracks are hidden by
default. YouTube is also the music-video source for every track,
including ones streamed from this cache, as a separate visible playback
under the YouTube embed rules in `AGENTS.md` (visible official IFrame
player, never audio-only or background playback). The PlayableSource
record (ADR 0012) is accepted and published (merged in #886) and is
the authoritative home for this order; the order stated above matches
it. If ADR 0012 is ever revised to state a different order, this
section is revised to match in the same change that adopts it.
Nothing in this record introduces offline downloads or weakens the one-concurrent-stream
lease: cached plays take the same lease and hour-meter writes as NNTP
plays.

**REA angle:** before the score weights are tuned on staging data,
study how shipped open-source music servers operate their on-disk
transcode/media caches (admission, size caps, eviction under
pressure) — source first, REA only for behaviour the source does not
settle — to learn the failure modes of popularity admission (one-hit
pollution, loop pinning) this design must avoid. That study informs
weight tuning in the implementation slices; it is not a blocker for
this record.

## Alternatives

- **Keep ADR 0003 grants unchanged:** rejected. The grant's only purpose
  was proof that a user's own provider fetched the bytes; with no user
  providers every grant would attest to the service's own fetch, a
  ceremony that still costs the full GC/gauge machinery.
- **Cache everything played (write-on-first-play):** rejected. One-off
  plays of the long tail would fill the cap with bytes nobody replays,
  evicting genuinely popular tracks and multiplying NNTP fills. Popularity
  admission spends both storage and pool bandwidth only on proven demand.
- **No cache:** rejected. Every repeat play re-fetches full FLAC articles
  through a small shared pool; connection and completion budgets (ADR
  0014) become the scaling limit for ordinary listening.
- **Per-user cache quotas inside the shared cache:** rejected. There is
  no user storage allowance in the current model; fairness between users
  is a playback concern (stream hours, concurrency), already handled
  outside the cache.

## Consequences and implementation alignment

Accepted design, not shipped behaviour. Required follow-up, each its own
gated change:

1. Plays write path lands first (it is also the popularity signal);
   `track_popularity` rollup + `popcache.*` config keys next.
2. Schema: global `track_media` (drop `user_id` semantics), drop
   `user_cache_grants`, remove ADR 0004 cache columns from `plans` once
   no reader remains. ADR 0002 and ADR 0004 need aligning revisions; this
   record is the authority for the cache parts in the meantime.
3. Stream worker: serve via catalogue + entitlement checks and the
   global mapping; retire the in-memory `track/<id>` pointer cache in
   favour of `track_media`-backed lookups. Ingest grant writers are
   deleted, not repurposed.
4. Cache-GC Workflow: watermark eviction, orphan sweep, art expiry —
   bounded batches, never on a play request.
5. Tests must cover: admission threshold boundaries (plays, distinct
   listeners, admin pin), fill coalescing, cap refusal at the high
   watermark, score/LRU eviction order, eviction racing a serve,
   missing-R2-object refill, and that no response distinguishes hit from
   miss. Security review covers the no-oracle rules before launch.
