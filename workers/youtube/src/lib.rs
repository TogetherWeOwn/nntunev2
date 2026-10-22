//! `nntune-youtube`: portable Rust core of the NNTune v2 YouTube backend.
//!
//! Day-1 spike slice (see `docs/youtube-backend-spike.md`): yt-dlp-style
//! extraction that runs fully on Cloudflare Workers, with a residential
//! proxy only for `/player` when the extraction IP is bot-walled — never
//! for streaming bytes.
//!
//! - [`client`]: Innertube client table (`VISIONOS` first) and `/player`
//!   request construction.
//! - [`player`]: `/player` response parsing: playability, stream URLs, and
//!   the `ip=` value the GO/NO-GO gate correlates against.
//! - [`probe`]: byte-probe planner (head 256 KiB, mid-file range, full
//!   fetch) for the gate measurement.
//! - [`gate`]: the GO/NO-GO verdict (byte-fetch failure rate from Worker
//!   egress, threshold 5%) and the JSONL measurement record.
//! - [`select`]: official-media selection (OMV/ATV gating, duration match,
//!   ATV-only audio choice, cache keys/TTLs).
//! - [`routes`]: fail-closed public fetch and extraction routing decisions,
//!   natively tested and rendered by the wasm32 [`runtime`] adapter.
//!
//! Portable logic plus the wasm32-only runtime entry: native builds and
//! `cargo test` never link the Workers runtime. Cloudflare bindings (DO,
//! KV) are declared in `workers/youtube/wrangler.toml.example`; the
//! extraction wiring, the `connect()` proxy client and the staging deploy
//! land with later Day-1 slices.

pub mod client;
pub mod gate;
pub mod player;
pub mod probe;
pub mod routes;
pub mod select;

#[cfg(target_arch = "wasm32")]
pub mod runtime;
