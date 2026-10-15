# ADR 0003: Cache Scope + R2 Layout

- **Status:** Accepted design; implementation and independent review gates remain.
- **Date:** 2026-10-13
- **Revised:** 2026-10-15 — explicit byte/retention budgets, live gauge,
  FK-safe reclamation and renewal-settlement-aware effective-plan lookup.

## Context

Repeated plays should not repeatedly fetch through a user's Usenet provider.
The choice is per-user object namespaces or shared storage deduplication with
per-user authorization. Provisioned per-environment buckets are
`nntunev2-{audio,art,archive}-{staging,prod}`; this ADR adopts those names.

## Decision

**Shared content-addressed storage with per-user serve grants.** A global
`cache_objects` row is a storage index, never authorization. Serving bytes
requires both a per-user `track_media` mapping and a
`user_cache_grants(user_id, content_hash)` row. A grant is created only after
that user's own provider fetched and hash-verified the bytes. Knowing a hash
or adding another user's track to a library confers no access. Per-user mappings
also prevent one user's bad match from poisoning everyone else's track.

`cached_hit` is server-side telemetry only. Do not return it or hit/miss headers,
expose hash-existence endpoints, or skip the own-provider proof because another
user has cached an object. These rules remove direct hit oracles; timing
resistance still needs security testing, not a claim based only on headers.

### R2 layout

```text
nntunev2-audio-{staging,prod}    # immutable audio bytes
  {sha256hex[0:2]}/{sha256hex[2:4]}/{sha256hex}
nntunev2-art-{staging,prod}      # deterministic artwork renditions
  {sha256hex[0:2]}/{sha256hex[2:4]}/{sha256hex}-{size}
nntunev2-archive-{staging,prod}  # retention-checked reserve, not scratch
```

Audio/art objects are immutable and hash-verified. Archive buckets have no Worker
binding by default. Scratch placement for ingest repair and MB dumps is not
decided here: the ops follow-up must pin placement/retention before using it.
No archive lifecycle expiry is assumed. The MB implementation must use the
environment-qualified art bucket, not an invented unsuffixed bucket.

### Writers and refetch

1. **Accept:** the API inserts an idempotent `ingest_jobs` row and increments
   the day's accepted-job count. No track id or media hash is available yet.
2. **Ingest `write`, after `match`:** commit the resolved `tracks`,
   `library_tracks` and per-user `track_sources` in one transaction. The source
   contains the parsed, ordered segment list (or a durable recoverable NZB
   manifest), not just an expiring download URL. An unmatched track gets a local
   id with NULL MBID. Ingest does not publish audio hashes or cache grants.
3. **Stream publication:** fetch through the requesting user's provider,
   verify the complete object's hash, then publish `cache_objects`,
   `track_media` and `user_cache_grants` for that user. Initialize both
   `last_served_at` columns at publication; refresh the grant and the served
   mapping on subsequent serves. Record the fetching `provider_id` on the grant;
   its composite FK binds it to the same user (ADR 0002). A metadata probe alone
   is not byte proof. Retain the provider row while grants reference it; revoke
   or evict those grants normally before deletion, never NULL out the proof.
4. **Missing grant or missing R2 object:** use `track_sources` to fetch through
   that user's provider again. No cross-user fallback. A missing library/source
   is 404; upstream unavailability is a retryable fetch error, not permission to
   borrow another user's grant. Eviction never deletes `track_sources`.

The source survives for the life of the user's track. Removing one library
membership must not remove it while the track remains in another of that user's
libraries. Removing the final membership deletes that user's source and mappings;
a grant can be released when no other mapping for that user needs its hash.
Sources use the versioned, row-bound encryption contract in ADR 0002.

### Accounting and collection

`plans.cache_bytes_budget` and `plans.cache_retention_days` have concrete values
in ADR 0004. A user's `cache_bytes` is a **live gauge**:

```sql
SELECT COALESCE(SUM(o.byte_size), 0)
FROM user_cache_grants g
JOIN cache_objects o ON o.content_hash = g.content_hash
WHERE g.user_id = $1;
```

The grant primary key counts each hash once per user. Sharing with another user
saves physical R2 space but does not discount either user's allowance. The gauge
is not in the daily `usage_ledger` and is never summed over a month.

The cache-GC Workflow runs in bounded batches and follows this order:

1. Identify grants outside their plan's retention window. For an over-budget
   user, additionally select least-recently-served grants until the remaining
   gauge fits the budget. Resolve the effective plan with **ADR 0002 rule 4**,
   including its fixed 3-hour active-paid renewal-settlement allowance and
   eligible renewal-failure grace. Paid period end alone is **not** a zero-budget
   signal. Only users without entitlement under that same predicate have a zero
   cache budget; this never deletes library metadata or sources. `cache_priority`
   orders work under shared storage pressure (lower tiers first); it cannot
   exempt a user from their own byte budget. An object larger than the user's
   entire budget is streamed without persistent cache admission.
2. Delete those grants, then orphaned `track_media` rows. Orphaned means the
   `(user_id, content_hash)` grant is absent or the track has left all of that
   user's libraries. Old mappings cannot independently retain objects forever:
   both timestamps are windowed and stale mappings are prunable. A recent grant
   still protects bytes used by another mapping for that user.
3. Sweep a `cache_objects` row/R2 object only with **zero grants and zero
   referencing mappings**. Serialize publication and sweep for that hash, and
   recheck references under that serialization before deleting bytes. DB FKs
   must not be disabled. Physical-delete failures are retried; they never
   restore a removed authorization. `ref_count` is advisory, not the mark set.

Evicting user A's grant frees A's gauge even if user B keeps the object alive.
Never delete B's grant merely because A reached a limit. In-flight reads must
handle a reclaimed object through the same own-provider refetch path. GC is
batch Workflow work (Containers-eligible), not unbounded work on a play request.

## Alternatives

- **Per-user prefixes:** simpler isolation, rejected for now because shared
  libraries can reduce physical R2 bytes. The grant model still requires every
  user's first own-provider fetch: it saves storage, **not Usenet quota**. No
  measured dedupe ratio is claimed. Revisit with measured savings/ops cost if
  the added grant machinery is not worthwhile.
- **Global mapping without grants:** rejected; permits cross-tenant content
  redistribution and cache poisoning.
- **No cache:** rejected; repeat plays unnecessarily consume provider quota.

## Consequences and implementation alignment

Stream and Subsonic cache readers must replace global `track/<track_id>` or bare
hash lookups with the per-user mapping and grant checks. The merged ingest
pipeline needs a follow-up `write` transaction for normalized library rows and
sources, not audio publication. These are implementation acceptance requirements,
not claims that current main already implements them. Tests must cover shared
hashes, final versus non-final membership removal, eviction/refetch, budget and
retention boundaries, and publication racing a sweep.
