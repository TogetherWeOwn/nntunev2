# ADR 0003: Cache Scope + R2 Layout

- **Status:** Accepted
- **Date:** 2026-10-13

## Context

Audio and artwork fetched via users' own Usenet providers must be cached so
repeat plays don't re-hit Usenet. Question: per-user cache namespaces, or a
shared content-addressed store with cross-user dedupe?

## Decision

**Shared content-addressed store with per-user authorization, not per-user
namespaces.** Bytes are identical regardless of whose provider fetched them, so
the cache key is `sha256(bytes)`, and `cache_objects` (ADR 0002) is the global
dedupe index. Access control is enforced at the API layer (only library tracks
a user may see resolve to cache keys), never by R2 key secrecy.

### R2 layout

```text
audio/{sha256hex[0:2]}/{sha256hex[2:4]}/{sha256hex}   # immutable audio bytes
art/{sha256hex[0:2]}/{sha256hex[2:4]}/{sha256hex}-{size}  # derived artwork renditions
tmp/{upload-id}/...                                   # ingest scratch, lifecycle-expired
```

- Objects are **immutable and content-addressed**: same bytes → same key.
  Overwrite never happens; corruption is detected by hash mismatch.
- `ref_count` in `cache_objects` is advisory (GC hint), not authoritative;
  collection is mark-and-sweep from `library_tracks` + recent `play_events`.
- Per-plan **cache priority** (ADR 0004) controls eviction order and prefetch
  depth, not visibility: higher tiers' working sets are swept last.

### Alternatives considered

- **Per-user prefixes (`u/{user_id}/...`):** rejected — duplicates identical
  bytes N times, multiplying R2 storage and Usenet egress with zero privacy
  gain (bytes are identical; authz belongs at the API layer).
- **No shared cache (always re-fetch):** rejected — defeats the product's
  offline-friendly goal and burns users' provider quotas.

## Consequences

- The stream worker verifies `sha256` on ingest before indexing; mismatches are
  quarantined, never served.
- Cache GC is a batch Workflow (Containers-eligible), never inline on play.
- Artwork renditions are derived deterministically (`{hash}-{size}`) so they
  dedupe too.
