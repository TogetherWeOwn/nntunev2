# nntune-core

Shared Usenet data-plane core for NNTune v2. Compiles for
`wasm32-unknown-unknown` (Cloudflare Workers via `workers-rs`) **and**
native targets (services, Container batch jobs).

| Module | What |
|---|---|
| `nntp` | NNTP client (RFC 3977): greeting, `MODE READER`, `AUTHINFO`, `GROUP`, `STAT`, `ARTICLE`, `BODY`, pipelined `fetch_many`; `430`/`481`/`502` mapped to typed errors |
| `transport` | Portable `Transport` byte-pipe trait + CRLF/dot-block framing; every impl enforces per-op timeouts as `TimedOut` |
| `transports::tokio_tls` | Native transport: tokio TCP + rustls (Mozilla roots), feature `native-tls` |
| `transports::worker_socket` | Workers transport over `connect()`, timeouts via `worker::Delay`, feature `worker-transport` (wasm32) |
| `yenc` | yEnc encode/decode, byte-identical to v1; explicit SIMD fast path (x86_64 SSE2, wasm simd128 with `RUSTFLAGS="-C target-feature=+simd128"`), scalar fallback |
| `nzb` | Streaming NZB parser (quick-xml) with hard bounds on files, segments, bytes, nesting; SHA-256 segment digest identical to v1 |
| `decrypt` | Ranged AES-CBC and rclone-crypt (`seal_block` helper for tests/tooling) |
| `par2` | PAR2 recovery-set parse + `verify` + CRC-recombine `precheck` (all wasm-safe); `repair` shells out to `par2cmdline-turbo` and is native-only |

## Features

- `default = ["native-tls"]` — what services and tests build.
- `native-tls` — tokio + rustls transport (non-wasm).
- `worker-transport` — Workers `connect()` socket transport (wasm32 only).

The crate itself never touches the network or the clock.

## Checks

```sh
cargo test -p nntune-core                    # 33 tests, incl. Go-vector interop
cargo clippy -p nntune-core --all-targets    # clean, `-D warnings` in CI
cargo bench -p nntune-core                   # yEnc ms/MB for the bench gate
cargo check -p nntune-core --target wasm32-unknown-unknown --no-default-features [--features worker-transport]
RUSTFLAGS="-C target-feature=+simd128" cargo check -p nntune-core --target wasm32-unknown-unknown --no-default-features
```

## Bench gate (this PR)

Native x86_64 (`cargo bench`, 1 MiB buffers):

| Case | Time | ms/MB |
|---|---|---|
| decode, clean (SIMD path) | 0.37 ms | **0.37** |
| decode, escape-heavy (scalar lanes) | 1.19 ms | **1.19** |
| encode | 2.54 ms | 2.54 |

Baseline: TS/V8 implementation at 1.2 ms/MB **on Workers**. Native Rust
decode beats it (0.37–1.19 ms/MB), but these are different machines and
runtimes — wasm-on-Workers was NOT measured here. The `workers/bench`
sibling card should run the same 1 MiB buffers through the deployed
simd128 build and report `Date.now()` deltas before this gate is called
fully closed.
