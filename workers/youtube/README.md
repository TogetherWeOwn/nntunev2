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
- `src/select.rs` — official-media selection (OMV/ATV gating, duration
  match, ATV-only audio choice, cache keys/TTLs).
- `src/routes.rs` — fail-closed public fetch and extraction routing
  decisions, natively tested.
- `src/runtime.rs` (wasm32 only) — the Worker entry: public fetch 404
  plus the `YtExtract` Durable Object (`POST /extract`, retryable 503
  until the extraction wiring lands).
- `wrangler.toml.example` — deploy contract: `YT_EXTRACT` binding +
  SQLite migration, `YT_CONFIG` / `YT_PLAYER_CACHE` KV placeholders.
- `corpus/day1.json` — HISTORICAL, retired: the original 100-video
  Day-1 corpus (50 VEVO label music videos, 50 auto-generated
  `- Topic` audio tracks). The VEVO/Topic substring method is
  superseded by the OMV/ATV selection rules; `corpus/build.sh` fails
  closed until rewritten. See `corpus/SOURCES.md` for the method and
  the pending re-cut.

Portable logic plus the wasm32-only runtime entry: native builds and
`cargo test` never link the Workers runtime (`worker` + `wasm-bindgen`
are wasm32-only dependencies).

## What's next (later Day-1 slices)

- The extraction/measurement wiring: `POST /extract` runs `/player`
  extraction, the public fetch serves measurement routes, and the YTM
  search + `/next` I/O lands for the corpus re-cut.
- The staging deploy (template → real `wrangler.toml` + provisioned KV)
  and the `connect()` proxy client. The proxy slice is parked until the
  dev path to the home residential endpoint is available; the runtime
  entry needs no proxy.
- The staging measurement run that produces the actual gate numbers: it
  must run from Worker egress, because extraction-IP reputation varies
  by egress (dev egress is fully bot-walled; see finding P1 in the
  spec).

## Checks

```sh
python3 scripts/cargo_cache.py run -- test -p nntune-youtube
python3 scripts/cargo_cache.py run -- clippy -p nntune-youtube --all-targets -- -D warnings
python3 scripts/cargo_cache.py run -- check -p nntune-youtube --target wasm32-unknown-unknown
python3 scripts/cargo_cache.py run -- clippy -p nntune-youtube --target wasm32-unknown-unknown -- -D warnings
```

(`cargo` itself lives in `/paperclip/.cargo/bin` and is not on `PATH`;
compiling commands always go through the wrapper.)
