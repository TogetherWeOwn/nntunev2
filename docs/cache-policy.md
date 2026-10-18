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

### Bucket sources (verified 2026-10-18)

- `nntunev2-audio-staging` / `nntunev2-audio-prod`: env and `AUDIO_CACHE`
  consumer per `workers/stream/wrangler.toml` (top-level prod binding,
  `env.staging` staging binding, `env.production` prod binding) and the
  `docs/ops.md` environments matrix plus worker-bindings row for `stream`.
  Object shape (`audio/<sha256>` blobs plus `track/<id>` pointers, deduped,
  immutable, no background reaper) per `workers/stream/src/cache.rs`. Edge
  `no-store` is prescribed but unwired: no `Cache-Control` string exists in
  `workers/stream/src`; the fetch entry answers `404` via
  `routes::decide_public_fetch` and `respond`/`respond_body` set only
  `content-type` (plus `retry-after` when carried); the portable
  `PlayResponse::cache_headers` emits only `Cache-Status` plus `Age`
  telemetry (see prescribed-vs-wired evidence). Lifetime (no object expiry;
  multipart-abort default only) per the TTL section and `docs/ops.md` R2
  lifecycle.
- `nntunev2-art-staging` / `nntunev2-art-prod`: env and `ART_CACHE`
  consumer per `workers/mb/wrangler.toml.example` (commented template:
  top-level plus `env.staging` staging name, `env.production` prod name;
  uncomment on activation) and the `docs/ops.md` environments matrix plus
  worker-bindings row for `mb`. Lookup/search JSON edge headers are wired
  per `workers/mb/src/lib.rs` (`LOOKUP_CACHE_TTL_S = 86400`,
  `SEARCH_CACHE_TTL_S = 300`, `cache_control`, `json_headers`,
  `cache_ttl == None` renders `no-store`, KV keys
  `mb:lookup:<kind>:<mbid>` and `mb:search:<kind>:<normalized q>` with
  matching TTLs). Art-byte `public, max-age=86400, immutable` is prescribed
  where unauthenticated serving lands; no R2 image-body serving path exists
  yet (entities carry `caa_url`; see deltas). Object shape
  (`art/{hh}/{hh}/{sha}-{size}`, immutable) per `caa_url_for_art` plus its
  tests and the `docs/adr/0003-cache-scope.md` R2 layout. Lifetime per the
  TTL section and `docs/ops.md`.
- `nntunev2-archive-staging` / `nntunev2-archive-prod`: reserved with no
  Worker consumer per the reservation guard below (CI-enforced). No
  `archive` `bucket_name` appears in `workers/stream/wrangler.toml`,
  `workers/mb/wrangler.toml.example` (commented art names only),
  `workers/repair/wrangler.toml` (`INGEST_SCRATCH` only),
  `workers/ingest/wrangler.toml.example` (`INGEST_SCRATCH` placeholder plus
  its never-bind-archive comment), or `apps/web/wrangler.toml` (no R2
  bindings); `docs/ops.md` lists both as reserved with no consumer and the
  binding manifest intentionally leaves archive unmapped. No reads, no
  bucket lock, and no object expiry per the TTL and guard sections.

## What "TTL" means here

No object-expiry lifecycle rule is configured on the six table buckets:
stored objects live until
explicitly deleted. Lifecycle is Cloudflare-side, not a Wrangler field,
so no checked-in manifest configures it; `docs/ops.md` records no object
expiry for the six. The only lifecycle rule observed in `docs/ops.md`
(metadata GETs 2026-10-17) is the enabled `Default Multipart Abort Rule`
on the archive buckets (age `604800` seconds, no prefix condition; it
aborts incomplete multipart uploads after 7 days and
does not expire completed objects). The two ingest-scratch buckets are
outside this table: they carry the enabled 7-day delete rule
`expire-ingest-scratch-after-7-days` on `ingest-scratch/` (age `604800`
seconds, action `deleteObjectsTransition`); `docs/ops.md` reports no
expiry outside that prefix. The TTL column therefore governs
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

Sources: `workers/mb/src/lib.rs` (`LOOKUP_CACHE_TTL_S`, `SEARCH_CACHE_TTL_S`,
`lookup_cache_key`, `search_cache_key`, `cache_control`, `json_headers`,
success-path `cache.put` with matching TTLs, `cache_ttl: None` for
failures).

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

CI enforces this via job `cache-policy` in `.github/workflows/ci.yml`
(archive-guard plus guard-unit-test steps; required in the `ci-ok` needs
list): `.github/scripts/check_archive_bindings.py` examines tracked TOML
files repo-wide (`git ls-files`, so custom config paths such as
`infra/archive.toml`, a root `wrangler.toml`, or `crates/x/wrangler.toml`
are covered and untracked dependency trees cannot hide manifests), and
rejects recognizable JSON Wrangler configs rather than silently skipping
them. Untracked files are ignored and commented-out example bindings are
ignored because TOML parsing skips comments. It inspects only
`r2_buckets` binding tables and fails when `bucket_name` or
`preview_bucket_name` contains
`archive` (case-insensitive), including per-environment overrides.
Coverage is pinned by
`.github/scripts/test_check_archive_bindings.py` (env-override,
preview-bucket, custom-path, outside-roots, untracked-ignored,
commented-ignored, and JSON-rejected cases; live-repo manifests pass).
The guard is name-based on purpose: renaming a bucket to dodge it breaks
the exact names in `docs/ops.md` first.

## Prescribed-versus-wired status (verified 2026-10-17)

Each row pins what this policy prescribes against what the edge serves
today. "Unwired" means no code path sets the header — it is a status
fact, not a request to change behavior. Bucket binding and
physical-bucket alignment stay with their owner lanes; this table records
header status only. This section changes no cache semantics, bucket
wiring, expiry, GC, or refill behavior.

| Bucket(s) | Prescribed edge `Cache-Control` | Wired behavior today | Evidence |
|---|---|---|---|
| `nntunev2-audio-staging`, `nntunev2-audio-prod` | `no-store` (this table above; deltas below) — authoritative for the real grant-gated play route | **Unwired on the Worker.** No `Cache-Control` string exists anywhere in `workers/stream/src`. The Worker fetch entry answers `404` until the play adapters land (`routes::decide_public_fetch`), and the `respond`/`respond_body` adapters set only `content-type` (plus `retry-after` when carried). The portable play core emits only `Cache-Status` (`HIT`/`MISS`/`STALE`) + `Age` via `PlayResponse::cache_headers` — freshness telemetry, not a browser/CDN directive, and the deploy-config slice must not wire `cache_headers()` onto edge responses without resolving ADR-0003 §"server-side telemetry only" (no hit/miss headers, to avoid hit oracles). **Separate synthetic demo (exempt):** the web helper `AUDIO_CACHE_CONTROL` (`private, max-age=60, must-revalidate`) is wired only on `/api/demo/audio` (no auth, no R2; pinned by `player-seek.spec.ts`), and is explicitly not the contract for the real play route. | `workers/stream/src/play.rs` (`PlayResponse::cache_headers`), `workers/stream/src/cache.rs` (`CacheStatus::header_value`), `workers/stream/src/routes.rs` (`PUBLIC_FETCH_STATUS`, `decide_public_fetch`), `workers/stream/src/runtime.rs` (`fetch`, `respond`, `respond_body`); demo-only: `apps/web/src/lib/player/cacheHeaders.ts` (`AUDIO_CACHE_CONTROL`), `apps/web/src/routes/api/demo/audio/+server.ts`, `apps/web/tests/player-seek.spec.ts`, `docs/adr/0003-cache-scope.md` (telemetry-only `cached_hit`) |
| `nntunev2-art-staging`, `nntunev2-art-prod` (lookup/search JSON) | `public, max-age=86400` (lookup), `public, max-age=300` (search), failures `no-store` | **Wired — matches this table.** Success responses carry `Cache-Control: public, max-age=<ttl>` via `cache_control(ttl)`; failures (`cache_ttl: None`) render `no-store` in `json_headers` and are written to neither cache. | `workers/mb/src/lib.rs` (`cache_control`, `json_headers`, and the `cache_ttl == None` assertions such as `lookup_missing_is_404_and_not_cached`; the only literal `no-store` header assertion is the healthz test) |
| `nntunev2-art-staging`, `nntunev2-art-prod` (art bytes) | `public, max-age=86400, immutable` wherever unauthenticated serving lands; gated art follows ADR-0003 (deltas below) | **Unwired on R2-backed art bytes.** No R2 image-body serving code path exists yet: the MusicBrainz Worker returns artwork URLs (e.g. `caa_url` on its entities) and never proxies full images inline, so there is nowhere to set the header on bucket-backed bytes. **Conflicting sources (both recorded, neither authoritative here):** ADR-0003 requires membership/entitlement-gated art to be served through the authenticated handler with `private`/`no-store`, not a public R2 URL — that rule wins for gated art. Separately, the synthetic web demo serves its placeholder via `ARTWORK_CACHE_CONTROL` (`public, max-age=31536000, immutable`) on `/api/demo/artwork` (no R2, no entitlement check; pinned by `cacheHeaders.test.ts` and `player-seek.spec.ts`), explicitly exempt from this prescription. | `workers/mb/src/lib.rs` (entities carry `caa_url`; no image-body response path), deltas below; `docs/adr/0003-cache-scope.md` (CAA/verified Bounds: authenticated handler, `private`/`no-store`); demo-only: `apps/web/src/lib/player/cacheHeaders.ts` (`ARTWORK_CACHE_CONTROL`), `apps/web/src/routes/api/demo/artwork/+server.ts` |
| `nntunev2-archive-staging`, `nntunev2-archive-prod` | n/a (no reads) | **n/a — no reads, no binding.** No Worker in this repository may bind an archive bucket (CI-enforced); neither stream nor MusicBrainz manifests target one. | `.github/scripts/check_archive_bindings.py` plus `cache-policy` job in `.github/workflows/ci.yml` (required in `ci-ok`); `workers/stream/wrangler.toml`, `workers/mb/wrangler.toml.example`, `workers/repair/wrangler.toml`, `workers/ingest/wrangler.toml.example`, `apps/web/wrangler.toml` |

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
  conformance target for the real play route. The web `AUDIO_CACHE_CONTROL`
  (`private, max-age=60, must-revalidate`, wired only on synthetic
  `/api/demo/audio` and pinned by `player-seek.spec.ts`) is an intentional
  demo exemption, not the play-route contract.
- Art bytes: the MusicBrainz Worker returns artwork URLs and never
  proxies full images inline, so no R2-backed art-byte `Cache-Control`
  code path exists yet. Art objects are immutable content-addressed blobs,
  so the prescription is `public, max-age=86400, immutable` wherever
  unauthenticated serving lands. Same conformance-target status as stream,
  with two recorded exceptions: membership/entitlement-gated art follows
  ADR-0003's authenticated-handler `private`/`no-store` rule (which wins
  over this row for gated art), and the synthetic `/api/demo/artwork`
  placeholder's `public, max-age=31536000, immutable` is an intentional
  demo exemption.
- Name-alignment deltas: none remaining. `workers/stream/wrangler.toml`
  binds the exact per-environment audio names and
  `workers/mb/wrangler.toml.example` references the suffixed art names.
  Neither targets an archive bucket, so the guard passes.
