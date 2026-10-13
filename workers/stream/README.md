# `stream` Worker + per-user NNTP pool

Portable Rust core (`nntune-stream`, zero dependencies) of the NNTune v2
stream Worker and its per-user NNTP pool Durable Object.

## Play path

```text
client ── play(token, range?) ──▶ stream Worker
  1. authorize: verify the signed grant from the api Worker (grant.rs)
  2. R2 hit: serve 200 / 206 from audio/<sha256> (cache.rs, range.rs)
  3. R2 miss: lease a pool connection (Playback lane) → BODY each segment
     over the warm connection (nntp.rs) → yEnc decode (yenc.rs) → decrypt
     → 16 KiB chunks to the client while staging the R2 multipart write;
     finalize() completes it into the cache (play.rs)
  4. prefetch the next track while pool capacity is idle (prefetch.rs)
  5. every step emits metering events (meter.rs)
```

Pool semantics (`pool.rs`): capacity = `min(provider_cap, plan_cap)` clamped
to `[1, 50]`; released connections stay warm (no per-play reconnect);
`Playback > Prefetch > Ingest` lanes order queued waiters; provider errors
quarantine the pool with 1s → 5s → 30s → 5min backoff, cleared on success.

## TTFB targets

First-byte latency measured in CI by `tests/stream_integration.rs`
(authorize → first chunk; `-- --nocapture` prints both):

| Path | Local reference (40 KB, loopback) | Staging p95 target |
|---|---|---|
| Cache hit | ~30 µs (2026-10-13 run) | ≤ 150 ms at the edge |
| Cache miss | ~90 ms (2026-10-13 run: dial + 2×BODY + decode) | ≤ 1.5 s |

Method: the integration test asserts the local hit budget (< 100 ms) on
every run; staging Darm-monitoring (Analytics Engine `PlayStart` →
first-byte events) owns the p95 numbers above. If the local hit assertion
ever goes red, treat it as a regression, not a flaky threshold.

## Test

```sh
cargo test -p nntune-stream
cargo clippy -p nntune-stream --all-targets   # must be warning-free
cargo fmt -p nntune-stream --check            # must be clean
```

No network, no Cloudflare account, no secrets: the synthetic NNTP server
lives in `tests/stream_integration.rs` and serves canned yEnc articles.

## Production notes

- Deploy bindings: `wrangler.toml` (`NntpPool` DO, `AUDIO_CACHE` R2,
  `METER` Analytics Engine dataset).
- Decryption in `play.rs` is a demo XOR stream cipher marking the
  fetch→decode→decrypt→fan-out shape. Production content keys arrive via
  the BYOP vault with the ingest-owned envelope (sibling cards); swap the
  marked lines, keep the flow.
- Metering sinks: Analytics Engine dataset `nntune_meter` + the Hyperdrive
  usage-ledger table (ledger writes owned by the api card).
