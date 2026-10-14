# ADR 0003: Cache Scope + R2 Layout

- **Status:** Accepted
- **Date:** 2026-10-13
- **Rev 2026-10-13:** addresses Paperclip Review on PR #2 — per-user serve
  authorization via grants (finding 1), provisioned bucket names + mirror-DB
  split (finding 8), GC over the new mapping tables (finding 3), no
  client-visible hit/miss signal (hit oracle).
- **Rev 2026-10-14 (review round 3):** bounded GC mark set (`last_served_at`
  window) + eviction deletes grants; archive buckets reserved (scratch
  placement open with ops owner); honest shared-vs-prefix trade-off with
  stream/Subsonic rework listed.

## Context

Audio and artwork fetched via users' own Usenet providers must be cached so
repeat plays don't re-hit Usenet. Question: per-user cache namespaces, or a
shared content-addressed store with cross-user dedupe?

R2 buckets are already provisioned per environment (ops plan, provisioning
card done): `nntunev2-{audio,art,archive}-{staging,prod}`. This ADR adopts
those names; it does not invent new ones.

## Decision

**Shared storage dedupe with per-user serve authorization — not per-user
namespaces, and not a globally readable cache.** Bytes are identical
regardless of whose provider fetched them, so the storage key is
`sha256(bytes)` and `cache_objects` (ADR 0002) is the global dedupe index.
But the index is storage only, never a capability: the stream worker serves
R2 bytes **only** on a matching `user_cache_grants(user_id, content_hash)`
row, created only after that user's own provider fetched and hash-verified
the object. Storage sharing is therefore invisible and unaddressable without
the grant row — user A's bytes are never served on user B's grant, which
reconciles threat-model §5 (the "namespace" is the grant set, not the key
prefix). Likewise the track→bytes mapping is per-user (`track_media`, ADR
0002): a global mapping would let one user's mis-matched or malicious ingest
poison that track for everyone.

`cached_hit` is server-side metering only — it is never returned to clients,
and responses carry no header distinguishing hit from miss, so a timing/size
difference can't become a cross-user fetch oracle.

### R2 layout (provisioned names + per-bucket key layout)

```text
nntunev2-audio-{staging,prod}    # immutable audio bytes
  {sha256hex[0:2]}/{sha256hex[2:4]}/{sha256hex}
nntunev2-art-{staging,prod}      # derived artwork renditions
  {sha256hex[0:2]}/{sha256hex[2:4]}/{sha256hex}-{size}
nntunev2-archive-{staging,prod}  # reserved archive capacity
                                 # (retention-checked; no Worker binding
                                 # by default — NOT scratch storage)
```

- Audio/art objects are **immutable and content-addressed**: same bytes →
  same key. Overwrite never happens; corruption is detected by hash mismatch.
- Ingest scratch (NZB working copies, par2 repair sets) and MB dump staging
  placement is **open — to confirm with the ops owner** (candidates: a
  dedicated scratch bucket, or `tmp/` in the audio bucket). This ADR does
  not place `tmp/` and claims no lifecycle expiry on the archive buckets,
  which the ops runbook treats as retention-checked.
- `ref_count` in `cache_objects` is advisory (GC hint), not authoritative.
  Collection is mark-and-sweep with a **bounded** mark set: `track_media` ∪
  `user_cache_grants` with `last_served_at` inside the retention window
  (per-plan, e.g. 30d Plus / 90d Pro). Grants older than the window are
  eviction candidates, not survivors — so the sweep actually reclaims.
  Eviction runs against the per-plan `cache_bytes` budget (`usage_ledger`,
  ADR 0004) in `cache_priority` order (lower tiers first; higher tiers'
  working sets are swept last), and eviction **deletes the grant rows**
  (plus orphaned `track_media` rows) so no dangling grants survive.
- Per-plan **cache priority** (ADR 0004) controls eviction order and prefetch
  depth, not visibility: higher tiers' working sets are swept last.
- Note: ADR 0007's `nntunev2-art` reference must gain the environment suffix
  above; owned by the MB card (non-blocking here).

### Alternatives considered

- **Per-user prefixes (`u/{user_id}/...`):** rejected — but on a narrower
  basis than first written. Under the grant model every user still fetches
  bytes through their own provider before receiving a grant, so shared
  storage saves **R2 storage only** (dedupe on overlapping libraries), not
  Usenet egress or provider quota. The kept benefit is paid for with the
  grant / `track_media` / GC-marking machinery, and per-user prefixes would
  give isolation by construction. Shared-plus-grants is kept because R2
  dedupe at our overlapping-library ratios outweighs that ops cost; if
  grant ops cost bites in practice, per-user prefixes are the named
  fallback (no new ADR needed to revisit, but the revisit must re-list the
  rework below).
- **Global mapping without grants (original draft of this ADR):** rejected on
  review — any user could add any track to their library and read bytes
  another user's provider fetched, redistributing BYOP content across
  tenants and breaking the BYOP premise; plus cross-user cache poisoning.
- **No shared cache (always re-fetch):** rejected — defeats the product's
  offline-friendly goal and burns users' provider quotas.

## Consequences

- The stream worker verifies `sha256` on ingest before indexing; mismatches are
  quarantined, never served. Indexing writes `track_media` + `user_cache_grants`
  for the fetching user only.
- Serving checks the grant row on every request; missing grant → fetch via the
  requesting user's own provider (or 404), never a cross-user byte serve. A
  granted-but-absent object (evicted between grant check and read) takes the
  same path: re-fetch via the requesting user's own provider (or 404) —
  never an error that leaks another tenant's state.
- Cache GC is a batch Workflow (Containers-eligible), never inline on play.
- Artwork renditions are derived deterministically (`{hash}-{size}`) so they
  dedupe too.
- **Known rework this ADR imposes on sibling cards:** the stream worker's
  global `track/<track_id>` pointer + signed-grant serve path and the
  Subsonic card's cache reads must move to the per-user
  `track_media` + `user_cache_grants` check; neither may serve on a bare
  content hash. Owned by the stream and Subsonic compat cards —
  non-blocking here, but their review must show the grant check.
