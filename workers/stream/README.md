# `stream` Worker + per-user NNTP pool

Rust core (`nntune-stream`) of the NNTune v2 stream Worker and its per-user
NNTP pool Durable Object. The Workers entry (`src/runtime.rs`) compiles on
wasm32 only; its `worker`, `wasm-bindgen`, and transport dependencies are
excluded from native builds and tests.

## Play path

```text
client ── play(token, range?) ──▶ stream Worker
  1. authorize: verify the signed grant from the api Worker (grant.rs)
  2. R2 hit: serve 200 / 206 straight from the shared cached blob
     (audio/<sha256>; cache.rs, range.rs) — no copy of the track
  3. R2 miss: lease a pool slot (Playback lane), take a warm connection
     (or dial one), BODY the first segment (or, for a mid-track seek with
     NZB segment ranges, the segment covering the range start), yEnc-decode it (multipart
     =ybegin/=ypart/=yend, pcrc32) and read the file size from =ybegin so
     Range/206 resolves before byte one. Later segments are fetched
     lazily as the client pulls 16 KiB chunks: decode → decrypt → client
     chunk + staged R2 multipart part (play.rs, `cache.rs`: segments
     coalesce into fixed 5 MiB R2 parts, the remainder stages). Memory =
     one 5 MiB part + one segment.
  4. finalize(): drain any segments the client's range skipped, complete
     the multipart write into the cache, hand the connection back warm.
     abort(): client left — return the connection, cache nothing.
  5. prefetch the next track while the pool has a spare slot (prefetch.rs)
  6. every step emits metering events to the `Meter` interface
     (`workers/stream/src/meter.rs`; hit/miss/finalize/abort paths in
     `workers/stream/src/play.rs`) — emitted events only, not deployed
     billing: the production sink is required-not-implemented under
     F-DEL-01 (`docs/security/threat-model.md` §9 row, §12 detail; owner
     mapping in the threat-model card thread, never pasted here). See
     “Metering reconciliation” below; §12 trust assumptions apply unchanged.
```

Pool semantics (`pool.rs`):

- capacity = `min(provider_cap, plan_cap)` clamped to `[1, 50]`.
- The pool **owns the idle `NntpClient`s**: a lease takes one back
  (`take_idle`) or the caller dials (`record_connect`); `release` parks the
  healthy connection again. Two uncached tracks in a row cost one dial
  (asserted against the synthetic server's own connection count). A warm
  connection the provider closed is redialed once, transparently.
- Lanes are enforced at acquire time (`Pool::reserve`, `Pool::try_acquire`):
  `Playback` may use every slot; `Prefetch` and `Ingest` leave one slot free
  for playback (so with a cap of 1 they never run outside the half-open probe
  below, which admits one `Playback` or `Ingest` acquire with no reservation).
  Waiter ordering across async tasks arrives with
  the Durable Object entry point; the reservation is what guarantees
  playback priority today.
- Only connection-level failures (I/O, protocol errors, read timeouts)
  (`NntpError::is_provider_failure`; settled per lease in `settle`)
  count toward the circuit breaker; a gone article (`430`/`423`) or a corrupt
  segment fails that one play (`ArticleMissing` / `Decode`) and returns the
  healthy connection. Breaker values are documented below.

## Upstream timeouts and circuit breaker

A dead upstream must fail fast with a `503`, never hang playback
(`nntp.rs`, `pool.rs`, `play.rs`):

| Setting | Value | Where |
|---|---|---|
| Dial (connect) timeout | 5 s default | `UpstreamTimeouts::default().connect` |
| Read timeout (idle read / total article response) | 10 s default | `UpstreamTimeouts::default().read` |
| Failure threshold | 3 consecutive provider failures | `pool::CIRCUIT_FAILURE_THRESHOLD` |
| Failure window | 30 s (older failures restart the streak at 1) | `pool::CIRCUIT_WINDOW_MS` |
| Open interval | 30 s | `pool::CIRCUIT_OPEN_MS` |

- Per-upstream timeouts ride `play_with_timeouts` (`play` uses
  `UpstreamTimeouts::default()`): `connect` bounds a new dial, and the
  current `read` timeout is applied when reusing a warm connection. They stay
  out of `PlayInput` on purpose so existing `PlayInput` literals keep
  compiling. A warm connection is reused only when its peer matches the
  current `server_addr`. `read` is both the idle-read limit and the total
  status-plus-body deadline for one
  article, so a trickling response cannot extend playback indefinitely. The
  first segment's connect, greeting, article and optional warm redial share a
  `connect + read` deadline; later articles each get a `read` deadline.
  The hung-upstream test asserts a silent article times out within that budget.
- The breaker opens after 3 consecutive provider failures inside the 30 s
  window. While open, `try_acquire` fails with `PoolQuarantined` without
  dialing out, and the play path maps it to **`503` with
  `Retry-After: 30`** (`PlayError::http_status` /
  `retry_after_header`) with the `upstream_circuit_open` envelope code. An exhausted
  pool (all slots leased) is `429` (`rate_limited`), not an outage. Auth,
  permission, range, missing-article and invalid-segment failures use their
  envelope statuses (`401`/`403`/`416`/`404`/`400`); provider failures are `502`.
- Once the 30 s open interval elapses the breaker is half-open: exactly one
  `Playback` or `Ingest` probe acquire is admitted, tracked independently of
  other live leases. `Prefetch` cannot consume it; an ingest probe may use the
  playback-reserved slot for this single recovery attempt. Concurrent acquires
  or a probe delayed by pool capacity are rejected with `Retry-After: 1`. A
  provider response (including a missing article) closes the breaker; a
  provider failure re-opens it for a fresh 30 s without resetting the failure
  streak. A failure that lands while the breaker is
  already open is held: the streak and the window are left alone, so the
  original open interval still half-opens for exactly one probe instead of
  silently closing. Settlement is lease-scoped: only the admitted
  probe's outcome closes or re-opens the breaker. A first-segment success
  settles only the probe and leaves a closed-state streak untouched, so a
  provider that serves segment 0 and dies mid-track still trips the breaker;
  the streak clears only when a play completes cleanly (`finalize` success
  or an `abort` with no failure). A stale lease's success clears a
  closed-state streak but never closes an open breaker, and an
  `abort` after a failed segment records that failure exactly like
  `finalize` would.
  The hung-upstream integration test trips the breaker at the threshold,
  asserts the `503` + `Retry-After` without touching the hung server, then
  probes recovery against the healthy server.

## Stampede guard (singleflight + stale-while-revalidate)

What is in this crate today: the singleflight **primitive**
(`src/stampede.rs`), the SWR cache tiers and headers, and a per-track
revalidation claim. What is **not** here yet: the wiring. Nothing in the
play path calls `Singleflight`, and no background revalidation runs until
the Worker / pool Durable Object entry point (deploy slice) lands and
consumes `needs_revalidate`. Until then, a stale entry serves inside its
SWR window and hard-misses after it — the coalescing below describes the
primitive the entry point will hold one-per-isolate, proven by its unit
tests (50 concurrent callers, exactly 1 upstream fetch), not live play-path
behaviour.

- Key: `flight_key(segment message-id)` — the normalized message-id only
  (same trim/`<>` strip as the wire). The upstream fetch is a whole-article
  `BODY` that ignores the client's `Range` header (`Range`/`206` is resolved
  against the fetched bytes), so seek storms asking for different ranges of
  one segment are the same fetch and share one flight. Sharing is in-flight
  only: once a flight's leader returns, later calls fetch again (freshness is
  the cache's job, below).
- Waiter timeout: `SINGLEFLIGHT_WAIT_TIMEOUT_MS = 5_000`. A waiter parked
  longer than 5 s re-runs leader election instead of fetching on its own:
  exactly one timed-out waiter replaces the flight and leads, the rest join
  the replacement. A replacement is never replaced for slowness (its waiters
  keep waiting in 5 s chunks), and no flight is led while a superseded leader
  is still fetching unless it is itself a replacement. So a stalled key runs
  at most **two upstream fetches at once**, however many callers pile up,
  and no waiter hangs behind a single wedged leader. If the replacement also
  stalls, its waiters can wait indefinitely in 5 s chunks (there is no overall
  deadline) until either fetch settles; callers needing a hard bound must add
  their own. When the original leader succeeds first, its result goes to the
  replacement's waiters too: the first
  success of the chain is the one every waiter takes. An error stays with the
  flight that produced it, so the original's failure never fails callers
  parked on a replacement that is still fetching. (The NNTP client's 10 s
  deadline bounds the full article operation, so a slow-but-healthy leader can
  be replaced once and briefly overlap its replacement; that bounded overlap
  is the documented trade-off.) A panicking leader releases its key, so later calls
  start a fresh flight; its waiters recover through their own timeout.
- Native only: the primitive parks waiters on a `Condvar`, which traps on
  `wasm32-unknown-unknown`, so the module is `#[cfg(not(target_arch =
  "wasm32"))]` and CI checks this crate on wasm32 to keep that fence
  honest. The Worker entry point needs an async shared-future /
  promise-map equivalent with the same key shape and timeout semantics.
- SWR windows (`src/cache.rs`): `CACHE_FRESH_TTL_SECS = 300` serves `HIT`
  with no revalidation; the next `CACHE_SWR_WINDOW_SECS = 3_600` serves
  `STALE` immediately while flagging `needs_revalidate` — to exactly one
  reader per stale entry, via `try_begin_revalidate` (an abandoned claim
  expires after `REVALIDATE_CLAIM_TTL_SECS = 300`; retain the acquisition
  timestamp when calling `finish_revalidate` so a late owner cannot release a
  newer claim). The claim also releases when the rewrite lands. Freshness is
  stamped per track, so deduplicated blobs never
  restamp a peer track. Past both windows the entry is a `MISS` and the
  play path re-fetches upstream. Cache writes are stamped with the play
  time on `finalize`, so the SWR clock runs from when the bytes were
  fetched.
- Headers: every play carries `Cache-Status: HIT|MISS|STALE` and
  `Age: <seconds-since-write>` (see `PlayResponse::cache_headers`).

## Play grants: TTL cap and clock skew (grant.rs)

Play grants (`v1.<payload>.<hmac-sha256>`) are short-lived bearer tokens:
a captured grant must stop replaying soon after issuance.

- `MAX_GRANT_TTL` = 900 s (15 min): the maximum grant lifetime.
- `CLOCK_SKEW_SECS` = 60 s: bounded clock-skew allowance. An issuer whose
  clock runs ahead of the verifier's produces grants that read as
  over-cap; up to 60 s of skew still verifies.
- Issuance clamps: `issue_at(user, track, now, ttl)` mints
  `exp = now + min(ttl, MAX_GRANT_TTL)` (saturating near `u64::MAX`), so a
  request beyond the cap is clamped to it rather than minted long-lived.
- Verification rejects: `verify` checks the signature first, then expiry
  (`now >= exp` stays `Expired` — skew never resurrects a grant), then the
  cap. A validly-signed, unexpired grant with `exp - now` beyond
  `MAX_GRANT_TTL + CLOCK_SKEW_SECS` (960 s) is rejected with the distinct
  code `GrantError::TtlTooLong`, so "minted too long" is distinguishable
  from `Expired`, `BadSignature` and `Malformed` in logs and callers. The
  play path surfaces it as `PlayError::Unauthorized(TtlTooLong)` without
  dialing out. Wrong-track binding is still rejected (`WrongTrack`) for
  within-cap grants.

## Grant-rejection meter signals (meter.rs, play.rs)

Authenticated grant rejections emit exactly one distinct meter event and
nothing else: no `PlayStart`, no cache events, no dial, no quarantine, no
lease. Malformed and bad-signature tokens emit no meter event, preventing
unauthenticated clients from driving meter writes.

- `GrantRejectedExpired` for `Unauthorized(Expired)`.
- `GrantRejectedExcessiveExpiry` for `Unauthorized(TtlTooLong)`.
- `GrantRejectedWrongTrack` for `WrongTrack` (with `track_id` set to the
  *requested* track).
- Rejection events carry the requested track, the user from the
  signature-verified payload, and `bytes = 0` — never secret or token
  material. The
  accept path emits no `GrantRejected*` at all.

## TTFB targets

First-byte latency is measured by `tests/stream_integration.rs`
(authorize → first chunk; `-- --nocapture` prints both):

| Path | Local reference (loopback) | Staging p95 target |
|---|---|---|
| Cache hit | ~25 µs (2026-10-14 run) | ≤ 150 ms at the edge |
| Cache miss (range from byte 0) | ~50 ms (2026-10-14 run: dial + first segment + decode) | ≤ 1.5 s |

Miss TTFB for a range starting at byte 0 is dial (skipped on a warm pool) +
**one** article fetch + decode, independent of track length: the integration
test asserts the server has received exactly one `BODY` when the first chunk
arrives on a three-segment track. A mid-track seek on a cold cache gets the
same one-`BODY` TTFB when `PlayInput::segment_ranges` carries the NZB
per-segment byte ranges: the article covering the range start is fetched
first, and the skipped prefix is backfilled in file order before serving
advances past the covering segment (or by `finalize` when the range ends
inside it), so multipart staging still only ever receives the contiguous
prefix — 5 MiB part coalescing is unchanged and at most the covering
segment is ever held out of order beyond the staged part. The whole track
still lands in cache. Ranges are a verified hint, never trusted blindly:
without ranges, with ranges that fail validation (wrong arity, gaps,
non-zero start), with a ranges-only `416`, or when the covering article
disagrees with its declared range, the play path keeps (or falls back to)
the prefix walk: it walks every earlier segment first and TTFB grows with
the seek position. Integration tests pin both costs (one
`BODY` vs the whole prefix) and byte-identical output. Staging p95 numbers
are owned by Analytics Engine monitoring (`PlayStart` → first-byte events)
once deployed.

CI (`.github/workflows/ci.yml`, job `rust`, required through `ci-ok`) runs
fmt, clippy `-D warnings` and every test; the integration test asserts the
local hit budget (< 100 ms), so a regression turns `ci-ok` red.

## Ingest probe (probe.rs, runtime.rs)

The ingest pipeline checks segment availability before verify/repair. One
call carries one batch envelope (the ingest crate's `ProbeRequest`, at most
500 message-ids) and returns which ids the provider has:

```text
ingest consumer ── NNTP_POOL stub (id_from_name(user_id)) ── POST /probe ──▶ NntpPool DO
  1. refuse envelopes naming another user (the DO id is the user id), bodies
     over 1 MiB, batches outside 1..=500, ids that are not strict message-ids
  2. lease on the Ingest lane (normally leaves one slot reserved for playback;
     a half-open recovery probe may use that slot)
  3. warm connection or dial; STAT each id: 223 present, 430 missing
  4. 200 {"batch_index", "present", "missing"}: the request ids, partitioned
```

| Status | Code | Meaning | Ingest |
|---|---|---|---|
| 200 | | outcome | check partition, fold into the tally |
| 400 / 413 | `bad_request` / `too_large` | envelope refused | contract bug, no retry |
| 503 | `busy` | ingest lane full or pool quarantined; `Retry-After` set | redeliver |
| 503 | `provider_unavailable` | provider settings not loaded | redeliver |
| 502 | `provider_error` | connection, auth or protocol failure; pool quarantined | redeliver |

There is no public route: only Workers holding the cross-script Durable
Object binding reach the object. Provider text never leaves the DO. A
warm connection the provider closed while idle is redialed once.

Not connected yet: the DO's provider settings. The encrypted BYOP row lives
in the account vault, and reading it inside the DO (Hyperdrive, the
key-encryption key, and the SSRF guard on every connect, threat model §3/§6)
is its own slice; until it lands every probe answers `503
provider_unavailable`. The ingest consumer configuration stays disabled
meanwhile.

## Test

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p nntune-stream --target wasm32-unknown-unknown -- -D warnings
cargo fmt --all --check
```

No Cloudflare account, no secrets: the play-path synthetic NNTP server lives
in `tests/stream_integration.rs` and serves canned yEnc multipart articles;
`tests/probe_synthetic_nntp.rs` runs the probe against the shared
`nntune-test-nntp` server over loopback TCP.

## Worker build and current runtime boundary

The crate builds the deployable Worker module with:

```sh
cargo build --manifest-path workers/stream/Cargo.toml --release --target wasm32-unknown-unknown
```

This emits `target/wasm32-unknown-unknown/release/nntune_stream.wasm` as well
as the Rust library artifact. Wrangler's build command runs `worker-build
--release` to generate the `build/worker/shim.mjs` entry configured below.
The wasm32-only `runtime` exports the `NntpPool` Durable Object. Its probe
route currently fails closed with `503 provider_unavailable` until provider
configuration is wired from the account vault; the Worker's public fetch path
remains `404` until the play path is connected to Cloudflare's R2 and metering
adapters. This is a buildable runtime entrypoint, not a production playback
deployment.

## Metering reconciliation (portable-core model)

The code in `meter.rs` models complete-play reconciliation and deduplication;
it is not yet wired into the production `play()` path or production sinks.
`UsageLedger` is an in-memory portable-core model, not a live Hyperdrive table,
and `reconcile` does not currently emit to Analytics Engine or call
`ReconcileMismatch::log_line`.

- Idempotency key: `derive_play_idempotency_key(user_id, track_id,
  play_id)` returns `play:{user}:{track}:{play}`. It rejects empty fields,
  unsafe log/key characters, and fields longer than 128 bytes for user IDs
  or 512 bytes for track and play IDs. Redelivery derives the same key; a
  genuine second listen carries a different `play_id`.
- Dedup rule: `UsageLedger::record` converges exact redeliveries to one
  in-memory entry — first write wins, mismatch is flagged, never silently
  adjusted. A redelivery that disagrees on user, track, or bytes, or a play
  whose key is not exactly `play:{user_id}:{track_id}:{play_id}` with valid
  fields, reports `RecordOutcome::Conflict` and changes nothing. The
  production Hyperdrive uniqueness constraint and upsert are not wired
  by this portable-core model.
- Reconcile pass: `reconcile(plays, ledger, meter)` checks each distinct
  play (first occurrence authoritative) against the ledger. A play whose
  ledger entry is missing, whose user or track identity differs, or whose
  metered bytes differ from the entry's bytes emits exactly one
  `ReconcileMismatch` meter event. A redelivery inside the same batch that
  conflicts with its first occurrence is flagged as its own mismatch.
  Mismatch events are non-serving and carry `bytes = 0` so byte sums over
  mixed event kinds cannot double-count; the flagged counts stay on the
  `ReconcileMismatch` record. `ReconcileMismatch::log_line()` formats
  a single-line representation with event `play_reconcile_mismatch` and
  both byte counts (`ledger_bytes=missing` when there is no entry). Identity
  fields are percent-escaped even if callers bypass constructor validation.
  The pass takes `&UsageLedger` and never adjusts, backfills, or drops an entry.
  Production wiring for the meter, log, and ledger remains future work.

## Production notes

- The breaker clock is virtual: the embedding Worker/DO must call
  `Pool::advance` with elapsed real milliseconds before time-based pool
  operations. This portable crate has no runtime clock driver, so open
  circuits will not recover automatically until the integration supplies it.
- Deploy bindings: `wrangler.toml` (`NntpPool` DO, `AUDIO_CACHE` R2,
  `METER` Analytics Engine dataset).
- Decryption in `play.rs` is a demo XOR stream cipher marking the
  fetch→decode→decrypt→fan-out shape. Production content keys arrive via
  the BYOP vault with the ingest-owned envelope (sibling cards); swap the
  marked lines, keep the flow.
- Planned metering sinks: Analytics Engine dataset `nntune_meter` (point
  shape in `src/meter.rs`: blobs `[kind, user_id, track_id]`, doubles
  `[bytes, seconds]`) and the Hyperdrive per-play table
  (`migrations/0019_stream_play_ledger.sql`, `stream_play_ledger` — not the
  ADR daily `usage_ledger`; row intents and statements owned by
  the api worker's `src/ledger.rs`, with the same `play:` key derivation and
  select+classify replay flow, fenced through the play owner's `users` row).
  Neither sink is wired by the portable-core
  reconciliation model described above. Every event carries stream seconds
  at the nominal 48 000 bytes/s rate. Billing folds per play, never across
  plays: the worker folds that play's bytes only (its own `served` counter
  into `CompletePlay.bytes`) and derives seconds from that play's total —
  per-event seconds each round up, so summing them overcounts split windows,
  and summing one `(user, track)` pair across play ids would bill one play
  for another's listen.
