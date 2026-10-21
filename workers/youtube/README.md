# `nntune-youtube`: YouTube backend spike core

Portable Rust core of the NNTune v2 YouTube backend: yt-dlp-style
extraction that runs fully on Cloudflare Workers, with a residential
proxy only for `/player` when the extraction IP is bot-walled — never
for streaming bytes. The spike spec (gate, architecture, day plan,
measured findings) lives in `docs/youtube-backend-spike.md`.

## What's here (Day-1 non-proxy slice)

- `src/client.rs` — Innertube client table (`VISIONOS` first, pinned to
  the upstream `youtubei.js` table) and `/player` request construction.
  The compiled-in try-order is the default; the Worker reads its ordered
  client list from KV config.
- `src/player.rs` — `/player` response parsing: playability class, plain
  stream URLs vs `signatureCipher`, `n`-param and SABR flags, and the
  `ip=` value the gate correlates against.
- `src/probe.rs` — byte-probe planner: head 256 KiB, centered 256 KiB
  mid-file range, full fetch.
- `src/gate.rs` — the GO/NO-GO verdict (byte-fetch failure rate from
  Worker egress, threshold 5%) and the per-video JSONL measurement
  record.
- `corpus/day1.json` — the 100-video Day-1 corpus (50 VEVO label music
  videos, 50 auto-generated `- Topic` audio tracks), built by
  `corpus/build.sh` via YouTube search + oEmbed verify/classify. See
  `corpus/SOURCES.md` for the method.

Pure portable logic: no network, no Worker bindings, no new external
dependencies (`serde` + `serde_json` only). Native builds and `cargo
test` never link the Workers runtime.

## What's next (later Day-1 slices)

- The wasm32 runtime entry (fetch handler, Durable Object, `wrangler.toml`
  + binding-manifest entry) and the `connect()` proxy client. The proxy
  slice is parked until the host procures proxy credentials (owner spend
  OK); the runtime entry needs no proxy.
- The staging measurement run that produces the actual gate numbers: it
  must run from Worker egress, because extraction-IP reputation varies
  by egress (dev egress is fully bot-walled; see finding P1 in the
  spec).

## Checks

```sh
python3 scripts/cargo_cache.py run -- test -p nntune-youtube
python3 scripts/cargo_cache.py run -- clippy -p nntune-youtube --all-targets -- -D warnings
```

(`cargo` itself lives in `/paperclip/.cargo/bin` and is not on `PATH`;
compiling commands always go through the wrapper.)
