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

**Shared content-addressed storage with per-user audio serve grants.** A global
`cache_objects` row is a storage index, never authorization. Serving **audio**
requires both a per-user `track_media` mapping and a
`user_cache_grants(user_id, content_hash)` row. A grant is created only after
that user's own provider fetched and hash-verified the bytes. Knowing a hash
or adding another user's track to a library confers no access. Per-user mappings
also prevent one user's bad match from poisoning everyone else's track.

**Artwork is library-scoped metadata, not audio.** It uses `release_art` and the
membership gate in ADR 0002 §6, never `track_media` or `user_cache_grants`.
No art grant exists, no provider id is fabricated for an HTTP fetch, and art
is excluded from the per-user `cache_bytes` gauge. Audio's grant rule is unchanged.

`cached_hit` is server-side telemetry only. Do not return it or hit/miss headers,
expose hash-existence endpoints, or skip the own-provider proof because another
user has cached an object. These rules remove direct hit oracles; timing
resistance still needs security testing, not a claim based only on headers.

### R2 layout

```text
nntunev2-audio-{staging,prod}    # immutable audio bytes
  {sha256hex[0:2]}/{sha256hex[2:4]}/{sha256hex}
nntunev2-art-{staging,prod}      # validated artwork, original only at launch
  {sha256hex[0:2]}/{sha256hex[2:4]}/{sha256hex}
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
2. **Ingest `write`, after `match`:** consume the server-produced, durably
   checkpointed accepted audio manifest (ADR 0002 rule 6); commit the resolved
   `tracks`, `library_tracks` and per-user `track_sources` in one transaction.
   Copy its per-audio file/track selection and ordered segments (or durable
   recoverable NZB), not just an expiring download URL. Unmatched, conflicting or
   unverifiable occurrences get local ids with NULL canonical MBIDs. Tenant tags
   and positions cannot populate shared catalog rows; only mirror-verified
   occurrences and MusicBrainz metadata can. This transaction publishes no hashes
   or cache grants. It inserts `release_art(release_id, rendition='original',
   source_kind, content_hash=NULL)` descriptors idempotently, without clearing an
   existing cache pointer on replay. `embedded` is valid only for a local release;
   `caa` only for a mirror-verified release. The Workflow durably checkpoints the
   resulting release ids and then advances to the separate `art` stage below;
   a crash before checkpointing replays the idempotent `write`, not a new import.
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

Retain the encrypted source while any of the user's libraries **or playlist
entries** references the track (ADR 0002 rule 3). Final library removal deletes
that user's media mappings and grants not needed by other current mappings, but
preserves the source for dormant playlist entries. They cannot read/stream until
the entry-keyed restore to an own library passes the same source and cap checks
(ADR 0002 §6); then a full
own-provider fetch rebuilds media/grants. Removing the last playlist reference
while no library holds the track deletes the retained source. Membership changes,
playlist/library/account deletion and source cleanup serialize on `(user_id, track_id)` and
recheck both reference sets in one transaction. Library/playlist/account deletion
is the explicit batched procedure in ADR 0002 rule 3, never a bare FK cascade:
enumerate affected pairs and release ids, lock in ascending release id, then
`(user_id, track_id)`, then hash order, and delete plus clean up per batch.
For local releases, acquire the
release serializer first; a source insert/delete/replace or a membership change
that alters the release's selected art source
also clears `release_art.content_hash`, `last_served_at`, `retry_after` and
`last_failure`, and advances `updated_at` in that transaction. Membership edits
that keep the same selected source leave the pointer alone. This covers final
playlist-reference cleanup as well as newer imports; it invalidates stale art
when its source is gone instead of waiting for retention. Without either reference, recovery
requires a new ingest, not a library edit or restore. Sources use ADR 0002's versioned,
row-bound encryption; retained sources do not retain cached bytes.

### Artwork acquisition, expiry and refill

The new idempotent Workflow **`art` stage runs after `write` commits**, before
job completion. Its principal is `ingest_jobs.user_id`, verified against the
checkpoint's job id; it has **no system bypass** of ownership, membership or
provider checks. It and an authenticated cover-art request use the same refill.
A per-release application-level serializer (not a SQL transaction) spans the
**entire refill**, including fetch/validation/PUT; source edits and pointer clears
use it too. Each attempt has a 30-second end-to-end timeout; a lost serialization
lease aborts publication, never continues as a stale writer. Network fetch, image
validation and R2 PUT happen **outside** SQL; a short publication transaction
upserts `cache_objects` and the nullable `release_art.content_hash` pointer. Store
both `r2_bucket` and `r2_key` (ADR 0002). Replays reuse the content-addressed object
and descriptor, not another release/art row. Optional art failure never rolls back
an imported library: at most three attempts, obeying the retry-after rule below,
then record unavailable art in the job result. A serve request makes at most one
attempt; a later authorized request may retry only once eligible.

- **Embedded/local:** require the caller's library membership, local ownership,
  and current entitlement under ADR 0002 rule 4 / ADR 0004 (trial or paid plan;
  no refill for paywalled or zero-entitlement callers). At most two concurrent
  art refills per user; extra authorized requests coalesce onto the in-flight
  attempt or return a retryable error without opening another provider fetch.
  The tag/fingerprint subphase of `match` (ADR 0002 rule 6) extracts the indicated
  picture while the audio bytes are already in hand and persists the validated
  bytes (or the bounded tag-header snapshot below) with the checkpoint, so the
  post-write `art` stage publishes without a second audio download in the common
  case. Otherwise select the lowest track id among the caller's current member
  tracks in that release whose encrypted `track_sources` includes
  `embedded_art_selector` (tag container/picture-block ordinal/MIME type, frozen
  in the accepted manifest). Fetch only a bounded prefix: the NZB segments
  covering the tag header up to an explicit 8 MiB cap, never a whole audio file
  or volume set; an archive-packed member that cannot yield its picture within
  that cap is `unavailable`, not retried as transient. Never use another user's
  source or a tenant URL. If the selected source was removed/replaced, recompute
  from current sources; if none has art, return unavailable. Snapshot
  `source_job_id` and recheck it, membership, and entitlement before publication;
  stale work cannot republish bytes for a superseded source. Only a source
  replacement/removal that changes the selected art source clears
  the local release's art pointer under the same release lock; a membership edit
  that keeps the same selected source leaves it alone. The next refill
  selects current sources. A playlist-only source does not authorize art reads.
  Three consecutive transient timeouts escalate to `unavailable` with the
  one-hour negative cache, so a 30-second attempt that can never finish does not
  flap every minute.
- **CAA/verified:** derive the front-image request from the trusted
  `releases.mbid`, not a claimed MBID or tenant URL. Use HTTPS and a fixed
  Cover Art Archive/image-host allowlist, validating redirects under the same
  network policy. No provider/grant is required, but the same entitlement check
  as embedded art applies before any upstream fetch. Tenant embedded art can never
  overwrite a verified release's art. Recheck the trusted release identity,
  the requesting user's membership, and entitlement before publication/serve.
- **Bounds:** accept only fully validated JPEG/PNG/WebP, at most 5 MiB and 16
  megapixels; reject other types, including SVG, and never trust the declared
  MIME alone. Launch stores only `original`; other sizes use that image, not
  unbounded transformations. Hash the validated bytes. Serve through the
  authenticated handler with private/no-store responses, not a public R2 URL.
- **Negative results:** under the release serializer, persist `last_failure` plus
  a fixed `retry_after`: unavailable/CAA 404/no valid embedded art waits one hour;
  transient fetch/timeout failure waits one minute. Until that deadline, member
  requests return the stored 404/70 or retryable error without upstream work;
  requests never slide the deadline. Both Workflow and serve refills obey it.
  Successful publication clears both fields. Local source/membership changes clear
  them with the cache pointer, so a new valid source need not wait. A failed stale
  attempt cannot write a negative result after its source or lease changes.
- **Retention:** initialize `last_served_at` at publication and refresh on each
  authorized serve. Art has a fixed **30-day idle window**, independent of plan
  audio budgets. GC clears stale `content_hash` pointers (and their timestamp)
  under the same release lock, but retains `source_kind`/release descriptors.
  A missing pointer **or missing R2 object** triggers the refill above after the
  membership check. CAA 404/no embedded image yields 404 / Subsonic 70; transport
  failures yield retryable errors. Never fall back to another user's local art.

Publication, reference clearing and sweep also share the per-hash serialization
used for audio. Acquire locks in release-then-hash order; a hash-only sweep never
acquires a release lock. Hold the hash serialization from R2 PUT through pointer
publication; recheck R2 existence before committing. This closes the PUT/GC gap
without doing HTTP inside SQL. A crash after PUT but before DB commit leaves an
unreferenced object: the bucket orphan sweep removes it only after 24 hours and a
serialized recheck of **all three** reference sets below. A crash after DB commit
is an idempotent replay. These are required implementation tests, not executed
runtime tests in this ADR.

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
3. Clear expired artwork pointers using the separate 30-day rule above; do not
   delete their durable descriptors. Audio budget/grant eviction never clears art.
4. Reconcile orphaned `track_sources`: delete rows with no `library_tracks` and
   no `playlist_tracks` row for that `(user_id, track_id)` (left behind by a
   crash between deletion batches), serialized per `(user_id, track_id)` with
   the release lock first, and invalidate the local art pointer in the same
   batch when the deleted source was the selected art source.
5. Sweep a `cache_objects` row/R2 object only with **zero `user_cache_grants`,
   zero `track_media` references and zero non-NULL `release_art.content_hash`
   references**. Art is explicitly in the mark set; its FK uses RESTRICT, not a
   cascading descriptor deletion. Serialize publication and sweep for that hash
   and recheck all three sets before deleting bytes from the stored bucket/key.
   DB FKs must not be disabled. Physical-delete failures are retried; they never
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
