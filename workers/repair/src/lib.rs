//! NNTune v2 repair Worker: the no-route front for the par2 repair
//! Container.
//!
//! Ingest reaches this Worker only through its `REPAIR_CONTAINER`
//! service binding (`POST /repair` with a `RepairRequest`). One
//! Durable Object per `job_id` owns the job's Container, streams the
//! staged `INGEST_SCRATCH` inputs in, runs one bounded repair, streams
//! outputs back to `out/` and writes `out/result.json` last, so a
//! repeated `(job_id, round)` never pays for a second run.
//!
//! [`contract`] is the pure, native-tested wire contract; the wasm
//! runtime lives in `runtime` (compiled only for `wasm32`).

pub mod contract;

#[cfg(target_arch = "wasm32")]
mod runtime;
