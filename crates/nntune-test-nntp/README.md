# nntune-test-nntp

Synthetic NNTP test server + fixture corpus for NNTune v2 CI. **No real
Usenet is used in CI.**

This crate is a re-export of `velmora-test-nntp` from
[velmora](https://github.com/TogetherWeOwn/velmora), pinned to the same
immutable git `rev` as `velmora` in `crates/nntune-core/Cargo.toml`. The
server, the runtime-generated CC0 corpus (synthesized sine tones), fault
injection, the `POST` spool and their acceptance tests live there; see
`crates/velmora-test-nntp/README.md` in velmora.

Cargo never runs a dependency's tests, so `tests/pinned_engine_boundary.rs`
keeps a representative check in this repository's gate: the pinned
`nntune_core` client fetches and decodes a multipart fixture and round-trips
a `POST` through the spool. A pin bump that regresses either fails here.

Existing callers keep the `nntune_test_nntp::` paths:

```rust
let server = nntune_test_nntp::TestNntpServer::spawn_for_test().await;
// connect a raw TCP NNTP client to server.addr(), AUTHINFO testuser/testpass
```

Default credentials: `testuser/testpass` (max 2 connections),
`otheruser/otherpass` (max 8).
