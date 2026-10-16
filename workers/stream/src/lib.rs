//! `nntune-stream`: portable Rust core of the NNTune v2 stream Worker.
//!
//! This crate holds the playlist-play business logic that runs inside the
//! Cloudflare `stream` Worker and its per-user NNTP pool Durable Object:
//!
//! - [`pool`]: one pool per user, capped at the user's provider connection
//!   cap (from BYOP settings and plan), with leases, priority lanes
//!   (playback > prefetch > ingest) and quarantine/backoff on provider errors.
//! - [`grant`]: signed play grants issued by the `api` Worker; the stream
//!   Worker authorizes every play against them.
//! - [`range`]: HTTP `Range` parsing / `206` resolution for seeking.
//! - [`cache`]: content-addressed audio cache + multipart write staging
//!   (the R2 shape; in-memory here).
//! - [`yenc`]: yEnc decode for NNTP segments.
//! - [`nntp`]: blocking NNTP `BODY` client for the miss path.
//! - [`play`]: miss-path orchestration (authorize → fetch → decode →
//!   decrypt → chunked stream + writeback).
//! - [`meter`]: Analytics Engine + usage-ledger events.
//! - [`prefetch`]: next-track prefetch on spare pool capacity (never the last connection).
//! - `stampede` (native only, compiled out on wasm32): singleflight
//!   stampede-guard primitive (one upstream fetch per concurrent segment)
//!   with waiter-timeout re-election. Not yet wired into the play path;
//!   wiring arrives with the Worker entry point.
//!
//! Cloudflare bindings (DO storage, R2, Analytics Engine) are wired in
//! `workers/stream/wrangler.toml`; this crate stays dependency-free so
//! `cargo test` runs anywhere. The `worker`/DO entry points that embed this
//! core arrive with the deploy-config slice.

pub mod cache;
pub mod grant;
pub mod meter;
pub mod nntp;
pub mod play;
pub mod pool;
pub mod prefetch;
pub mod range;
// Native only: the Mutex/Condvar waiter path traps on wasm32-unknown-unknown
// (and a single-threaded isolate cannot park a waiter anyway). The Worker
// entry point gets an async shared-future equivalent; see the module docs.
#[cfg(not(target_arch = "wasm32"))]
pub mod stampede;
pub mod yenc;
