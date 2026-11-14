//! Shared Usenet data-plane core for NNTune v2.
//!
//! The engine modules are re-exported from `velmora`
//! (TogetherWeOwn/velmora, pinned by git `rev` in this crate's
//! `Cargo.toml`); their sources, tests and benches live there. This crate
//! keeps only the NNTune-specific [`transcode`] and [`original_source`]
//! modules and their tests.
//!
//! Compiles for `wasm32-unknown-unknown` (Cloudflare Workers via `workers-rs`)
//! **and** native targets (ingest/streaming services, Container batch jobs).
//!
//! Modules:
//!
//! - [`article`]: posting article assembly — header builder with folding,
//!   per-article obfuscation, newsgroup allow-list selection, cipher-block
//!   payload sizing.
//! - [`rng`]: CSPRNG bytes (`getrandom`, `js` backend on wasm) behind an
//!   injectable [`rng::FillRandom`] source.
//! - [`nntp`]: NNTP client over a portable [`transport::Transport`] trait.
//! - [`pinned_transport`]: pinned, cancellable fetch policy (TLS-on only,
//!   port allowlist, cooperative cancellation, names-only diagnostics).
//! - [`yenc`]: yEnc encode/decode with an explicit SIMD fast path (x86_64 SSE2,
//!   wasm32 simd128) and scalar fallback, plus CRC32.
//! - [`nzb`]: streaming NZB parser with hard resource bounds.
//! - [`media_layout`]: bounded article ranges derived from the media v1 geometry.
//! - [`media`]: `velmora.media.v1`, the media encryption format (age v1
//!   STREAM payload, 48-byte committing header): sealing, commitment-gated
//!   ranged reads and full verification. NNTune uses
//!   [`media::Profile::nntune`] (Random key mode only).
//! - [`par2`]: PAR2 recovery-set parsing and verification, plus the creator
//!   (GF(2^16) Reed-Solomon, one set per file at 10%). Repair is native-only
//!   (it shells out to `par2cmdline-turbo` in a Container).
//! - [`segment_identity`]: the one shared segment message-id rule (250-byte
//!   wire ceiling, bare shape, no normalization) enforced identically by
//!   ingest intake, the probe/body envelopes, play, and the wire adapters.
//! - [`transcode`]: the `opus-64` profile contract (pinned ffmpeg argv,
//!   Ogg serial normaliser, content-addressed artifact keys) shared by
//!   the ingest dispatcher and the transcode Container.
//!
//! The protocol/parser core uses injected transports. Transport implementations
//! own network I/O and per-operation timeouts (see [`transport`] and
//! [`transports`]); the native repair runner owns its subprocess deadline.

pub use velmora::{
    article, error, media, nntp, nzb, par2, pinned_transport, rng, segment_identity, transport,
    transports, yenc, Error, Result,
};

pub mod media_envelope;
pub mod media_layout;

pub mod original_source;
pub mod transcode;
