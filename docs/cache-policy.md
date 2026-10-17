# NNTune v2 cache policy (R2 TTL / eviction + archive reservation)

Authoritative TTL and eviction policy for the six audio/art/archive R2
buckets in the table below (`docs/ops.md` also inventories two
ingest-scratch buckets, which are outside this table — see below).
Bucket names below are exact; Worker
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

No object-expiry lifecycle rule is configured on the six table buckets:
stored objects live until
explicitly deleted. Cloudflare currently has a default lifecycle rule on
all six that aborts
incomplete multipart uploads after 7 days; it
does not expire completed objects. The two ingest-scratch buckets are
outside this table: they carry a 7-day delete rule on `ingest-scratch/`
and no multipart-abort rule. The TTL column therefore governs
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

## Prescribed-versus-wired status (verified 2026-10-17)

Each row pins what this policy prescribes against what the edge serves
today. "Unwired" means no code path sets the header — it is a status
fact, not a request to change behavior. Bucket binding and
physical-bucket alignment stay with their owner lanes; this table records
header status only. This section changes no cache semantics, bucket
wiring, expiry, GC, or refill behavior.

| Bucket(s) | Prescribed edge `Cache-Control` | Wired behavior today | Evidence |
|---|---|---|---|
| `nntunev2-audio-staging`, `nntunev2-audio-prod` | `no-store` (this table above; deltas below) | **Unwired.** No `Cache-Control` string exists anywhere in `workers/stream/src`. The Worker fetch entry answers `404` until the play adapters land (`routes::decide_public_fetch`), and the `respond`/`respond_body` adapters set only `content-type` (plus `retry-after` when carried). The portable play core emits only `Cache-Status` (`HIT`/`MISS`/`STALE`) + `Age` via `PlayResponse::cache_headers` — freshness telemetry, not a browser/CDN directive. | `workers/stream/src/play.rs` (`PlayResponse::cache_headers`), `workers/stream/src/cache.rs` (`CacheStatus::header_value`), `workers/stream/src/routes.rs` (`PUBLIC_FETCH_STATUS`, `decide_public_fetch`), `workers/stream/src/runtime.rs` (`fetch`, `respond`, `respond_body`) |
| `nntunev2-art-staging`, `nntunev2-art-prod` (lookup/search JSON) | `public, max-age=86400` (lookup), `public, max-age=300` (search), failures `no-store` | **Wired — matches this table.** Success responses carry `Cache-Control: public, max-age=<ttl>` via `cache_control(ttl)`; failures carry `no-store` and are written to neither cache. | `workers/mb/src/lib.rs` (`cache_control`, the header-append sites, and the `no-store` test assertion) |
| `nntunev2-art-staging`, `nntunev2-art-prod` (art bytes) | `public, max-age=86400, immutable` wherever serving lands | **Unwired.** No art-byte serving code path exists yet: the MusicBrainz Worker returns artwork URLs (e.g. `caa_url` on its entities) and never proxies full images inline, so there is nowhere to set the header. | `workers/mb/src/lib.rs` (entities carry `caa_url`; no image-body response path), deltas below |
| `nntunev2-archive-staging`, `nntunev2-archive-prod` | n/a (no reads) | **n/a — no reads, no binding.** No Worker in this repository may bind an archive bucket (CI-enforced); neither stream nor MusicBrainz manifests target one. | `.github/scripts/check_archive_bindings.py`, `workers/stream/wrangler.toml`, `workers/mb/wrangler.toml.example` |

## Header-consistency deltas (verified 2026-10-16)

- MusicBrainz JSON headers match this table exactly (lookup 86400,
  search 300, errors `no-store`); no delta.
- Stream audio edge `Cache-Control` is **prescribed but not yet wired**:
  the Worker fetch entry point exists, but no play route exists yet to set
  response headers; the deploy-config slice owns that implementation.
  Audio plays are grant-authorized
  per user and metered per request, so the prescription is `no-store`:
  browser replay must not skip grant-expiry checks, logout handling, or
  usage metering. The deploy-config slice applies it; this table is the
  conformance target.
- Art bytes: the MusicBrainz Worker returns artwork URLs and never
  proxies full images inline, so no art-byte `Cache-Control` code path
  exists yet. Art objects are immutable content-addressed blobs, so the
  prescription is `public, max-age=86400, immutable` wherever serving
  lands. Same conformance-target status as stream.
- Name-alignment deltas: none remaining. `workers/stream/wrangler.toml`
  binds the exact per-environment audio names and
  `workers/mb/wrangler.toml.example` references the suffixed art names.
  Neither targets an archive bucket, so the guard passes.
