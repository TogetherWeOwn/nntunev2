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
//! - [`observability`]: Analytics Engine row + structured JSON log line for
//!   a play (cache hit/miss, TTFT, bytes, pool error code; hashed identity),
//!   projected from the play outcome; the Worker entry point that writes it
//!   arrives with the deploy-config slice (see the module docs).
//! - [`play`]: miss-path orchestration (authorize → fetch → decode →
//!   decrypt → chunked stream + writeback).
//! - [`meter`]: Analytics Engine + usage-ledger events.
//! - [`prefetch`]: next-track prefetch on spare pool capacity (never the last connection).
//! - [`probe`]: ingest segment-availability probe (`STAT` on the ingest
//!   lane), served by the `NntpPool` Durable Object.
//! - [`routes`]: fail-closed public fetch and probe routing decisions, natively
//!   tested and rendered by the wasm32 [`runtime`] adapter.
//!
//! Cloudflare bindings (DO storage, R2, Analytics Engine) are wired in
//! `workers/stream/wrangler.toml`. The Workers entry (`runtime`, wasm32 only)
//! exports the `NntpPool` Durable Object with its probe route. Native builds
//! and `cargo test` never link the Workers runtime.

pub mod cache;
pub mod grant;
pub mod meter;
pub mod nntp;
pub mod observability;
pub mod play;
pub mod pool;
pub mod prefetch;
pub mod probe;
pub mod range;
pub mod routes;
pub mod yenc;

#[cfg(target_arch = "wasm32")]
pub mod runtime;
