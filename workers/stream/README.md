# `stream` Worker + per-user NNTP pool

Portable Rust core (`nntune-stream`, zero dependencies) of the NNTune v2
stream Worker and its per-user NNTP pool Durable Object.

## Play path

```text
client ── play(token, range?) ──▶ stream Worker
  1. authorize: verify the signed grant from the api Worker (grant.rs)
  2. R2 hit: serve 200 / 206 straight from the shared cached blob
     (audio/<sha256>; cache.rs, range.rs) — no copy of the track
  3. R2 miss: lease a pool slot (Playback lane), take a warm connection
     (or dial one), BODY the FIRST segment, yEnc-decode it (multipart
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
  6. every step emits metering events (meter.rs)
```

Pool semantics (`pool.rs`):

- capacity = `min(provider_cap, plan_cap)` clamped to `[1, 50]`.
- The pool **owns the idle `NntpClient`s**: a lease takes one back
  (`take_idle`) or the caller dials (`record_connect`); `release` parks the
  healthy connection again. Two uncached tracks in a row cost one dial
  (asserted against the synthetic server's own connection count). A warm
  connection the provider closed is redialed once, transparently.
- Lanes are enforced at acquire time: `Playback` may use every slot;
  `Prefetch` and `Ingest` always leave one slot free for playback (so with a
  cap of 1 they never run). Waiter ordering across async tasks arrives with
  the Durable Object entry point; the reservation is what guarantees
  playback priority today.
- Only connection-level failures (I/O, protocol errors) quarantine the pool
  with 1s → 5s → 30s → 5min backoff, cleared on success. A gone article
  (`430`/`423`) or a corrupt segment fails that one play
  (`ArticleMissing` / `Decode`) and returns the healthy connection.

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
arrives on a three-segment track. A mid-track seek on a cold cache instead
walks every earlier segment first (TTFB grows with the seek position):
`PlayInput` carries only message-ids, so without NZB segment sizes the play
path cannot know which article covers an offset — covering-segment fetch is
a follow-up once ingest exposes per-segment byte ranges. An integration test
pins the current prefix cost. Staging p95 numbers are owned by Analytics
Engine monitoring (`PlayStart` → first-byte events) once deployed.

CI (`.github/workflows/ci.yml`, job `rust`, required through `ci-ok`) runs
fmt, clippy `-D warnings` and every test; the integration test asserts the
local hit budget (< 100 ms), so a regression turns `ci-ok` red.

## Test

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

No network, no Cloudflare account, no secrets: the synthetic NNTP server
lives in `tests/stream_integration.rs` and serves canned yEnc multipart articles.

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

## Production notes

- Deploy bindings: `wrangler.toml` (`NntpPool` DO, `AUDIO_CACHE` R2,
  `METER` Analytics Engine dataset).
- Decryption in `play.rs` is a demo XOR stream cipher marking the
  fetch→decode→decrypt→fan-out shape. Production content keys arrive via
  the BYOP vault with the ingest-owned envelope (sibling cards); swap the
  marked lines, keep the flow.
- Metering sinks: Analytics Engine dataset `nntune_meter` + the Hyperdrive
  usage-ledger table (ledger writes owned by the api card).
