# ADR 0003: Cache Scope + R2 Layout

- **Status:** Accepted
- **Date:** 2026-10-13
- **Rev 2026-10-13:** addresses Paperclip Review on PR #2 — per-user serve
  authorization via grants (finding 1), provisioned bucket names + mirror-DB
  split (finding 8), GC over the new mapping tables (finding 3), no
  client-visible hit/miss signal (hit oracle).

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
nntunev2-archive-{staging,prod}  # ingest scratch (NZB working copies, par2
                                 # repair sets) + MB dump staging;
                                 # lifecycle-expired
  tmp/{upload-id}/...
```

- Audio/art objects are **immutable and content-addressed**: same bytes →
  same key. Overwrite never happens; corruption is detected by hash mismatch.
- `ref_count` in `cache_objects` is advisory (GC hint), not authoritative;
  collection is mark-and-sweep from `track_media` ∪ `user_cache_grants`
  (reachable hashes survive; everything else is swept).
- Per-plan **cache priority** (ADR 0004) controls eviction order and prefetch
  depth, not visibility: higher tiers' working sets are swept last.
- Note: ADR 0007's `nntunev2-art` reference must gain the environment suffix
  above; owned by the MB card (non-blocking here).

### Alternatives considered

- **Per-user prefixes (`u/{user_id}/...`):** rejected — duplicates identical
  bytes N times, multiplying R2 storage and Usenet egress with zero privacy
  gain (bytes are identical; the real boundary is the grant check at serve
  time, not the key prefix).
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
  requesting user's own provider (or 404), never a cross-user byte serve.
- Cache GC is a batch Workflow (Containers-eligible), never inline on play.
- Artwork renditions are derived deterministically (`{hash}-{size}`) so they
  dedupe too.
