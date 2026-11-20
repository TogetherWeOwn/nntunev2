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

`src/provider_authority_store.rs` is an inert model, not a module: only
`tests/provider_authority_store_acceptance.rs` includes it (see
`docs/design/provider-authority-store/README.md`).

## The velmora pin

`Cargo.toml` pins velmora by an immutable git `rev`, never a branch or tag;
`crates/nntune-test-nntp` pins `velmora-test-nntp` to the same rev. Engine
changes land as a velmora PR first, then a pin bump here that moves both
revs and every committed lockfile together (root, `workers/api`,
`workers/ingest`, `workers/repair`, `workers/transcode`). `deny.toml`
allows velmora as the only git source.

## Features

Forwarded to velmora unchanged:

- `default = ["native-tls"]` — what services and tests build.
- `native-tls` — tokio + rustls transport (non-wasm).
- `worker-transport` — Workers `connect()` socket transport (wasm32 only).

## Checks

CI runs on GitHub Actions (see repo `AGENTS.md`); the engine's own tests,
benches and bench gate run in velmora's CI. A focused local reproduction of
one failing test here goes through the bounded Cargo pool:

```sh
python3 scripts/cargo_cache.py run -- test -p nntune-core <fully_qualified_test_name> -- --exact
```
