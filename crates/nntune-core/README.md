# nntune-core

Shared Usenet data-plane core for NNTune v2. Compiles for
`wasm32-unknown-unknown` (Cloudflare Workers via `workers-rs`) **and**
native targets (services, Container batch jobs).

| Module | What |
|---|---|
| `nntp` | NNTP client (RFC 3977): greeting, `MODE READER`, `AUTHINFO`, `GROUP`, `STAT`, `ARTICLE`, `BODY`, pipelined `fetch_many`; `430`/`481`/`502` mapped to typed errors |
| `transport` | Portable `Transport` byte-pipe trait + CRLF/dot-block framing; every impl enforces per-op timeouts as `TimedOut` |
| `pinned_transport` | Pinned, cancellable fetch policy for NZB fetch: TLS-on only, port allowlist, cooperative cancellation, names-only diagnostics (see below) |
| `transports::tokio_tls` | Native transport: tokio TCP + rustls (Mozilla roots), feature `native-tls` |
| `transports::worker_socket` | Workers transport over `connect()`, timeouts via `worker::Delay`, feature `worker-transport` (wasm32) |
| `yenc` | yEnc encode/decode, byte-identical to v1; explicit SIMD fast path (x86_64 SSE2, wasm simd128 with `RUSTFLAGS="-C target-feature=+simd128"`), scalar fallback |
| `nzb` | Streaming NZB parser (quick-xml) with hard bounds on files, segments, bytes, nesting; SHA-256 segment digest identical to v1 |
| `decrypt` | Ranged AES-CBC and rclone-crypt (`seal_block` helper for tests/tooling) |
| `par2` | PAR2 recovery-set parse + `verify` + CRC-recombine `precheck` (all wasm-safe); `repair` shells out to `par2cmdline-turbo` and is native-only |

## Pinned transport proof (NZB fetch)

`nntune_core::pinned_transport` is the fail-closed policy the NZB intake
wiring consumes (TOG-26211, untouched):

- **Allowlist:** `ALLOWED_NNTP_PORTS = [119, 563]` (the NNTP conventions).
  The policy additionally requires TLS-on, so `119` is only valid as a TLS
  port here, never as plaintext.
- **TLS-first order:** `validate_nntp_endpoint` checks `tls` BEFORE the port
  allowlist. A TLS-off config is `TlsRequired` whatever the port — even an
  off-allowlist one — so port policy never launders a plaintext config into
  a port error. Pure validation: no I/O, no DNS, no clock.
- **Cancel bound:** `CANCEL_BOUND_MS = 1_000`. `with_cancel(fetch, cancel)`
  resolves the fetch unless `cancel` resolves first (fetch wins ties). A
  cancelled session has unknown wire state: discard the client and
  reconnect, never reuse it. The fetch future is dropped at cancel. The
  native `transports::tokio_tls::connect_pinned` entry point runs the same
  validation before any TCP dial (the general `connect` stays permissive for
  the plaintext loopback fixtures used by the stream/ingest harnesses and
  client unit tests; NZB fetch must use `connect_pinned`).
- **Stable codes:** `TransportRejection::code()` returns
  `transport_tls_required` (`TlsRequired`), `transport_port_not_allowed`
  (`PortNotAllowed(port)` — the payload never changes the code), and
  `transport_cancelled` (`Cancelled`). The native connect error embeds the
  same code in its `io::Error` message (`InvalidInput`).
- **Names-only diagnostics:** `Display`/`Debug` for `TransportRejection` and
  `ValidatedEndpoint` render host, port and code material only — never the
  configured username or password. Shared path is `std`-only (cancellation
  via `std::future::poll_fn`, no `tokio`), so the wasm32 check keeps
  passing.

## Features

- `default = ["native-tls"]` — what services and tests build.
- `native-tls` — tokio + rustls transport (non-wasm).
- `worker-transport` — Workers `connect()` socket transport (wasm32 only).

The protocol/parser core uses injected transports; the transport implementations
and the native repair runner own network I/O and deadlines.

## Resource limits and lookup behavior

- **NNTP:** `NntpConfig.max_article_bytes` defaults to 64 MiB, counting CRLF
  but excluding bytes already in the output buffer. Overflow rolls back partial
  output and makes the connection unusable. A failed `fetch_many` also poisons
  the client; reconnect before retrying. Successful results are all buffered, so
  limit the requested batch size to your runtime's memory budget.
- **NZB:** `document_bytes` is enforced at the `BufRead` boundary, including
  during buffering of a single XML event. The input's own buffer is separate.
- **PAR2:** Original `filename_bytes` determine identity. Ambiguous display-name
  lookups return `None`; use `file_by_name_bytes`, `verify_bytes`, and
  `precheck_bytes` for byte-precise selection and verification.

**Native repair is Linux-only**, requiring `/usr/bin/prlimit`, a trusted
image-pinned binary, and an exclusive scratch directory. Default ceilings are
120 seconds wall time, 60 seconds CPU, 1 MiB combined output, 8 GiB workspace
(and per-file size), and 512 MiB address space. `repair_with_limits` accepts
smaller budgets. The wrapper runs `par2 repair -q` (single `-q`: `Repair
complete.` / `All files are correct, repair is not required.` kept, progress
suppressed; `-q -q` would silence the real binary entirely; the `<N> recovery
blocks` count line is suppressed at `-q`, so `repaired_blocks` is 0 when
nothing was needed and -1 when repair completed or the count is unknown).
`RepairReport` carries `exit_code`/`signal` for retry vs dead-letter decisions
(par2cmdline-turbo: 0 success; 1/2/4 permanently unusable inputs and 6 with
`Recovery will not be possible` a corrupt set — dead-letter, do not retry;
only 8 and signal kills are resource failures. Any other code (e.g. 3, 5, 7,
or 6 without that line) is unexpected: bound retries and run `par2 verify`
first to separate unusable sets from transient failures). The calling Container must
remain unprivileged with quota-backed scratch: aggregate-size polling is
neither a sandbox nor a hard disk quota.

## Checks

```sh
cargo test -p nntune-core                    # incl. Go-vector interop
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
