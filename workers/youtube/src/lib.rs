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
//!
//! This crate is pure portable logic: no network, no Worker bindings.
//! Native builds and `cargo test` never link the Workers runtime. The
//! wasm32 runtime entry, KV/DO bindings and the `connect()` proxy client
//! land with the deploy-config slice.

pub mod client;
pub mod gate;
pub mod player;
pub mod probe;
pub mod select;
