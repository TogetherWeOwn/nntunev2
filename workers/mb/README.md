# workers/mb — MusicBrainz mirror read API

Rust (`workers-rs` → `wasm32`) read API over the `nntunev2-mb` Postgres
database (via Hyperdrive). Schema: `migrations/0001_mb_mirror.sql`
(mirror tables plus the single-row `mb_import_state` snapshot),
`0016_mb_track.sql` (`mb_track` listing source) and
`0017_mb_replication_packet_ledger.sql` (per-packet SLO ledger).
Decision: `docs/adr/0007-mb-mirror.md`.

## Endpoints

| Method | Path | Notes |
|---|---|---|
| `GET` | `/healthz` | Unauthenticated Worker health check; `{"ok":true}`, `Cache-Control: no-store`; does not access bindings |
| `GET` | `/v1/mb/artist/:mbid` | Artist lookup over Hyperdrive (`MB_DB`); success `Cache-Control: public, max-age=86400` |
| `GET` | `/v1/mb/release/:mbid` | Release lookup (includes the stored `caa_url`); same caching as artist |
| `GET` | `/v1/mb/recording/:mbid` | Recording lookup; same caching as artist |
| `GET` | `/v1/mb/release/:mbid/recordings` | Recordings of a release in album order (`medium_no`, then `track_no`, then MBID), listed from `mb_track` membership, `LIMIT 1000`; existing release with no recordings is `200` with an empty list, missing release is `404`; same caching as lookups |
| `GET` | `/v1/mb/search?q=&type=&limit=&offset=` | `type` in `artist\|release\|recording`; `q` 2-100 chars; trigram (`pg_trgm`) ordered by similarity; paged: `limit` defaults to `MAX_PAGE_SIZE` (25), over-limit clamped (never rejected), `limit=0` is `400`, `offset` defaults to 0, past-the-end is a 200 with `"results": []`; success `Cache-Control: public, max-age=300` |

(Table sources: routes `workers/mb/src/lib.rs:1035-1062`; healthz
`lib.rs:423-430,952-961`; lookup SQL/TTL `lib.rs:38-39,128-150`;
recordings SQL `lib.rs:154-170` plus repo `lib.rs:777-798`; search
validation/SQL `lib.rs:544-612,183-206`; `Cache-Control` rendering
`lib.rs:846-875`.)

Errors are JSON (`{"code", "message", "retryable"}`) with `Cache-Control: no-store`:
malformed `:mbid` (must be UUID), unknown `type`, and short/long/control-character queries
are `400` (`invalid_request`); empty/whitespace `q` is a stable coded `400`
(`MB_SEARCH_EMPTY_QUERY`, registry: `docs/api-errors.md`), never a scan;
a well-formed MBID with no row and unknown routes are `404`
(`not_found`); mirror read failures are a generic `500` (`internal_error`, fixed
`"unexpected error"` message — no DB detail leaves the worker).
(Sources: `workers/mb/src/api_error.rs:17-41,66-110`;
`docs/api-errors.md:67-76`; unknown-route handler `lib.rs:1019-1031`.)
Lookup success is the flat entity fields plus `"kind"`;
search success is `{"q", "type", "limit", "offset", "results": [{"mbid", "name"|"title",
"score"}]}`; release results also carry `artist_mbid`/`artist_name`
(null when the release has no artist) so callers can verify
"Artist - Album" queries against the artist instead of the title-only
score; release-recordings success is `{"release_mbid",
"recordings": [{"mbid", "release_mbid", "title", "track_no",
"duration_ms", "medium_no"}]}`. The listing reads `mb_track`
(`migrations/0016_mb_track.sql`), which stores every track appearance
with its medium position — unlike `mb_recording.release_mbid`, which
keeps one canonical release per recording and would drop a reissue's
shared tracks; a bare recording lookup answers `medium_no: null`.
Interactive reads never call musicbrainz.org.
(Sources: `"kind"` tag `workers/mb/src/lib.rs:293-318`; search hits
`lib.rs:321-359`; recordings envelope `lib.rs:489-516`; null medium on
bare lookups `lib.rs:139-146`; `mb_track` rationale
`migrations/0016_mb_track.sql:1-21,23-31` and `lib.rs:156-170`;
Hyperdrive-only reads `lib.rs:719-798`.)

## Caching

- Order per read: edge Cache API, then KV, then Hyperdrive Postgres.
  (Sources: edge check `workers/mb/src/lib.rs:877-880`; KV-then-DB cores
  `lib.rs:461-483,489-516,544-612`; edge store `lib.rs:864-875`.)
- Lookups: KV key `mb:lookup:<kind>:<mbid>` (lowercase MBID), 24 h TTL;
  the same TTL is served as `Cache-Control` and stored via the Cache API.
  (Sources: key `lib.rs:243-245`; TTL `lib.rs:38`; headers `lib.rs:846-854`.)
- Release recordings: KV key `mb:release-recordings:<mbid>` (lowercase
  MBID), same 24 h TTL and edge treatment as lookups. An existing
  release with zero recordings is a cacheable success; a missing
  release (`404`) is not cached.
  (Sources: key `lib.rs:249-251`; core `lib.rs:489-516`.)
- Search: KV key `mb:search:<kind>:<normalized q>:<limit>:<offset>`
  (query trimmed, lowercased, whitespace collapsed; SQL binds the same
  normalized value), 5 min TTL, so one page can never serve another
  page's cached body.
  (Sources: normalizer `lib.rs:235-240`; keys `lib.rs:243-251,530-535`;
  TTL `lib.rs:39`; core `lib.rs:544-612`.)
- Only successes are cached (a successful search with zero hits counts).
  400/404/500 responses are `no-store` and written to neither cache.
  KV failures degrade to plain DB reads; a missing `MB_CACHE` binding
  disables KV, a missing/unreachable `MB_DB` yields the generic `500`.
  (Sources: success-only envelope `lib.rs:397-421`; KV degrade/no-op
  `lib.rs:675-708`; connect/query errors map to the generic 500
  `lib.rs:719-775` and `src/api_error.rs:101-103`.)
- Release responses currently preserve the stored `caa_url` unchanged, so
  lookup cache keys remain environment-independent. When public R2 hosts are
  activated and URLs become environment-specific, include the environment in
  lookup cache keys before rewriting `caa_url`.
  (Sources: `caa_url` read through unchanged `lib.rs:812-819`; rewrite
  helper unused by responses `lib.rs:81-89`; env-free keys `lib.rs:243-251,530-535`.)
- Cover art bytes are intended for the per-environment R2 art buckets
  (`nntunev2-art-staging` / `nntunev2-art-prod`, `docs/ops.md` inventory;
  key form `art/{hh}/{hh}/{sha}-{size}` per `docs/cache-policy.md:17` and
  the `caa_url_for_art` helper doc example `art/ab/cd/<sha>-500`
  `lib.rs:81-88`; ADR 0003 shows only the base hash layout without the
  `art/` prefix and `-{size}` suffix). Public release `caa_url`
  generation remains deferred until bucket bindings and public hosts are
  activated; this worker never proxies full images inline.
  (Sources: buckets `workers/mb/src/lib.rs:61-65`;
  `workers/mb/wrangler.toml.example:21-26,38-52`;
  `docs/ops.md:63-64,330,373`;
  `docs/adr/0003-cache-scope.md:37-42`; JSON-only routes `lib.rs:1035-1062`.)

## p95 methodology (acceptance input)

Staging measures, per endpoint, over 1k requests (50 concurrent) against
staging Hyperdrive + KV:

- `lookup p95 < 250 ms` (cache miss) / `< 50 ms` (cache hit)
- `search p95 < 600 ms` (trigram, `LIMIT 25`)

Numbers are recorded here once staging exists; this slice defines the
method, not the measurements.
(Source: `docs/adr/0007-mb-mirror.md:44-46` — numbers land in this README
once staging measures exist.)

### Replication SLO

The SLO contract is the per-packet rule from ADR 0007: **each packet must
be applied within 2 hours of publication**. The evaluator does not calculate
or report a replication-lag p95. The **p95 source**, if a manual p95
statistic is computed per the operational procedure (which must define its
population and reporting window), is the `lag_secs` value of each applied
packet in `mb_replication_packet`, not the aggregate `mb_import_state`
snapshot; a manual p95 over the observed packet records in the reporting
window is only a reporting statistic, never the alert rule (a p95 under 2
hours would tolerate 5% of packets over budget). Published but
not-yet-applied packets have no `lag_secs` yet, so a manual p95 excludes
them. The **breach definition** is any packet with `lag_secs > 7200` seconds (exactly 7200 passes), or any missing sequence between the lowest observed packet and the reported latest packet sequence whose publication is either unknown or more than 7200 seconds before evaluation. A missing sequence recorded as pending
(`record_published`) that is still inside its 2-hour budget is in flight,
not a breach: it counts as `pending_count` instead, so a normal hourly
apply window does not fire the breach counter. Missing packets cannot be
dropped from the accounting: each breaching one is returned as a
`missing_sequence` breach, capped at 10,000 enumerated breaches per
evaluation (with one `missing_sequence_truncated` breach past the cap, so
truncation is never silent). An empty ledger evaluated against a
non-negative latest sequence is an `empty_ledger` breach, not a pass: with
no applied packet there is no baseline, and a total stall must not read as
healthy.
(Sources: per-packet rule `docs/adr/0007-mb-mirror.md:24-26`; lag check
`workers/mb/src/replication.rs:186-194` (`>` so exactly 7200 passes);
gap enumeration `replication.rs:205-274`; pending in-budget
`replication.rs:231-242`; 10,000 cap `replication.rs:8-21,288-326`;
empty ledger `replication.rs:196-204`; ledger schema
`migrations/0017_mb_replication_packet_ledger.sql:3-14` versus the
single-row snapshot `migrations/0001_mb_mirror.sql:57-66`.)

`ReplicationSloProbe` returns a JSON structured event named
`mb_replication_slo_breach` for every evaluation, including `breach_count`
(equal to the full breach list on the report), `missing_count`,
`pending_count` and a `truncated` flag. The logged `breaches` array is
bounded to stay well under the 256 KB Workers per-request log limit:
contiguous unknown-publication missing sequences compress into one
`missing_sequence_range` entry (`first`/`last`/`count`) and at most 100
entries are logged, so the truncation evidence is never the part cut off.
The report carries `metric_name` (`mb_replication_slo_breach_total`). No
runner exists yet: a scheduled runner must call `evaluate`, emit
`structured_log`, and increment that counter by `breach_count` when
non-zero. This crate only provides the evaluation. The durable
ledger schema is `migrations/0017_mb_replication_packet_ledger.sql`; packet
application remains the separate replication-applier responsibility.
(Sources: event/log builder `workers/mb/src/replication.rs:335-411`;
report `replication.rs:133-144,276-284`; metric name `replication.rs:20-21`;
log cap `replication.rs:14-19`.)

## Terms

Core MusicBrainz data is CC0; CAA images follow their per-image terms.
Commercial use notes the MetaBrainz support tier (see ADR 0007).

## Dev

```sh
cp wrangler.toml.example wrangler.toml  # fill ids locally, never commit
cargo fmt --check --manifest-path workers/mb/Cargo.toml \
  && cargo clippy --manifest-path workers/mb/Cargo.toml --target wasm32-unknown-unknown -- -D warnings \
  && cargo test --manifest-path workers/mb/Cargo.toml
```
