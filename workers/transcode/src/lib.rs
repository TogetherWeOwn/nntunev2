//! NNTune v2 transcode Worker: the no-route front for the `opus-64`
//! transcode Container.
//!
//! Ingest reaches this Worker only through its `TRANSCODE_CONTAINER`
//! service binding (`POST /transcode` with a `TranscodeRequest`). One
//! Durable Object per `job_id` owns the job's Container, streams the
//! staged `INGEST_SCRATCH` input in, runs one bounded encode, streams
//! the artifact back to `out/` and writes `out/result.json` last, so
//! a repeated `(job_id, staging_id)` never pays for a second run.
//!
//! [`contract`] is the pure, native-tested wire contract; the wasm
//! runtime lives in `runtime` (compiled only for `wasm32`).

pub mod contract;

#[cfg(target_arch = "wasm32")]
mod runtime;
