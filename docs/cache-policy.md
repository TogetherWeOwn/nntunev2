# NNTune v2 cache policy (R2 TTL / eviction + archive reservation)

Authoritative TTL and eviction policy for the provisioned R2 buckets
inventoried in `docs/ops.md`. Bucket names below are exact; Worker
configs must use them verbatim. This document creates no buckets,
renames nothing, and binds nothing — it pins how long entries live and
which buckets a Worker may never consume.

## Buckets

| Bucket | Env | Consumer | Edge `Cache-Control` on reads | R2 object lifetime / eviction |
|---|---|---|---|---|
| `nntunev2-audio-staging` | staging | stream Worker (`AUDIO_CACHE`) | prescribed `no-store`; not yet wired (see deltas) | persistent content-addressed blobs (`audio/<sha256>` + `track/<id>` pointers); no object expiry; incomplete multipart uploads abort after 7 days |
| `nntunev2-audio-prod` | prod | stream Worker (`AUDIO_CACHE`) | prescribed `no-store`; not yet wired (see deltas) | same as staging |
| `nntunev2-art-staging` | staging | MusicBrainz Worker artwork cache (`ART_CACHE`) | `public, max-age=86400, immutable` prescribed where art bytes are served; lookup/search JSON headers below are implemented | persistent immutable objects (`art/{hh}/{hh}/{sha}-{size}`); no object expiry; incomplete multipart uploads abort after 7 days |
| `nntunev2-art-prod` | prod | MusicBrainz Worker artwork cache (`ART_CACHE`) | same as staging | same as staging |
| `nntunev2-archive-staging` | staging | **none — reserved; no Worker binding in this repo (CI-enforced)** | n/a (no reads) | reserved; no bucket lock configured; no object expiry |
| `nntunev2-archive-prod` | prod | **none — reserved; no Worker binding in this repo (CI-enforced)** | n/a (no reads) | reserved; no bucket lock configured; no object expiry |

## What "TTL" means here

No object-expiry lifecycle rule is configured: stored objects live until
explicitly deleted. Cloudflare currently has a default lifecycle rule on
all six buckets that aborts incomplete multipart uploads after 7 days; it
does not expire completed objects. The TTL column therefore governs
**downstream copies only** (edge Cache API, KV, browser/CDN via
`Cache-Control`), never completed R2 objects. Deleting or overwriting an
R2 object does not purge edge/client copies already served; those linger
up to their served `max-age`.

Implemented edge headers (verified in code):

- MusicBrainz lookups: `public, max-age=86400`, backed by KV key
  `mb:lookup:<kind>:<mbid>` with a matching 24 h TTL.
- MusicBrainz search: `public, max-age=300`, backed by KV key
  `mb:search:<kind>:<normalized q>` with a matching 5 min TTL.
- MusicBrainz failures (400/404/500): `no-store`, written to neither
  cache. A successful search with zero hits counts as a success.

Audio objects are content-addressed and immutable: identical plaintext
uploaded for two tracks is stored once, and a `track/<id>` pointer swap
never mutates a blob. Eviction is therefore always safe to defer —
stale pointers cannot exist, only orphaned blobs, which an explicit
operator pass reclaims. There is no background reaper.

## Archive reservation guard

The two archive buckets are reserved capacity. They are not evidence of
an archival pipeline, not scratch space, and not a fallback cache. No
Worker in this repository may declare an R2 binding whose `bucket_name`
or `preview_bucket_name` targets an archive bucket, in any supported
manifest or environment override. This is a repository CI policy, not a
Cloudflare access-control boundary: it does not prevent writes through API
or S3 credentials, or from Workers deployed outside this repository. No
bucket lock is configured on either archive bucket.

CI enforces this: `.github/scripts/check_archive_bindings.py` examines tracked
TOML files repo-wide so custom config paths with R2 bindings are covered, and
rejects recognizable JSON Wrangler configs rather than silently skipping them.
Untracked files are ignored. It inspects only `r2_buckets` binding tables and
fails when either bucket name contains
`archive` (case-insensitive), including per-environment overrides. The
guard is name-based on purpose: renaming a bucket to dodge it breaks the
exact names in `docs/ops.md` first.

## Header-consistency deltas (verified 2026-10-15)

- MusicBrainz JSON headers match this table exactly (lookup 86400,
  search 300, errors `no-store`); no delta.
- Stream audio edge `Cache-Control` is **prescribed but not yet wired**:
  `workers/stream` currently ships the portable cache/orchestration core
  only — no Worker entry point exists to set response headers, and the
  deploy-config slice owns that file. Audio plays are grant-authorized
  per user and metered per request, so the prescription is `no-store`:
  browser replay must not skip grant-expiry checks, logout handling, or
  usage metering. The deploy-config slice applies it; this table is the
  conformance target.
- Art bytes: the MusicBrainz Worker returns artwork URLs and never
  proxies full images inline, so no art-byte `Cache-Control` code path
  exists yet. Art objects are immutable content-addressed blobs, so the
  prescription is `public, max-age=86400, immutable` wherever serving
  lands. Same conformance-target status as stream.
- Name-alignment deltas (owned by the binding-alignment slices, not
  this policy): `workers/stream/wrangler.toml` still binds the legacy
  `nntune-audio-cache` name without environment sections, and
  `workers/mb/wrangler.toml.example` references a commented-out
  nonexistent `nntunev2-art` name. Neither targets an archive bucket, so
  the guard passes; the names must still be aligned to this table.
