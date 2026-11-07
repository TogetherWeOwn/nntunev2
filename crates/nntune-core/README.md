# nntune-core

NNTune v2's Usenet core. The engine itself (NNTP client, transports,
yEnc, NZB, ranged decrypt, PAR2, segment identity, posting articles)
lives in [velmora](https://github.com/TogetherWeOwn/velmora); this crate
re-exports it under the existing `nntune_core::` paths and keeps only the
NNTune-specific modules. Compiles for `wasm32-unknown-unknown` (Cloudflare
Workers via `workers-rs`) **and** native targets (services, Container
batch jobs).

| Module | Where |
|---|---|
| `article`, `decrypt`, `error`, `nntp`, `nzb`, `par2`, `pinned_transport`, `rng`, `segment_identity`, `transport`, `transports`, `yenc` (+ `Error`, `Result`) | Re-exported from velmora; see `crates/velmora/README.md` there for the module table, resource limits, pinned-transport policy and repair contract |
| `transcode` | Local: the `opus-64` profile contract (pinned ffmpeg argv, Ogg serial normaliser, content-addressed artifact keys) |
| `original_source` (+ `reader`) | Local: original-source references and the segment reader built on `nzb` and `yenc` |
| `pool_channel`, `pool_operation` | Local, retained: the provider-pool channel and operation types (see the module docs in `src/pool_channel.rs` and `src/pool_operation.rs`); Worker dialing remains disabled |

`src/provider_authority_store.rs` is an inert model, not a module: only
`tests/provider_authority_store_acceptance.rs` includes it (see
`docs/design/provider-authority-store/README.md`).

## The velmora pin

`Cargo.toml` pins velmora by an immutable git `rev`, never a branch or tag.
Engine changes land as a velmora PR first, then a pin bump here moves all
shim revs and affected committed lockfiles together (root, `workers/api`,
`workers/ingest`, `workers/repair`, `workers/transcode`). `deny.toml`
allows velmora as the only git source.

The draft restack targets `5af1bfe901d15b5f041a5d1973b866436d988b19` for
`velmora`, `velmora-test-nntp` and `velmora-ssrf-guard`. It is incomplete
until the Actions-generated locks and all three shim manifests land together;
the existing pin does not supply the new auth/qualification APIs yet.

## Features

Forwarded to velmora:

- `default = ["native-tls"]` — what services and tests build.
- `native-tls` — tokio + rustls transport (non-wasm).
- `worker-transport` — Workers `connect()` socket transport (wasm32 only).
- `worker-pinned-tls` — qualification-only forwarding to velmora's
  `worker-pinned-tls` (implies `worker-transport`); no runtime call site is
  enabled by this feature and Worker dialing remains disabled.

## Checks

CI runs on GitHub Actions (see repo `AGENTS.md`); the engine's own tests,
benches and bench gate run in velmora's CI. A focused local reproduction of
one failing test here goes through the bounded Cargo pool:

```sh
python3 scripts/cargo_cache.py run -- test -p nntune-core <fully_qualified_test_name> -- --exact
```
