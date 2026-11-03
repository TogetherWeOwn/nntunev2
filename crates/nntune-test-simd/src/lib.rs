//! Execute the nntune-core simd128 kernels where their vector dispatch is live.
//!
//! The yEnc decoder fast path and the PAR2 creator `gf16::madd` kernel select
//! their implementation by target: natively they run scalar code, while
//! `wasm32` builds with `target-feature=+simd128` (the Workers deploy flags)
//! run the vector path. The differential tests in `tests/simd_differential.rs`
//! assert kernel output against independent scalar oracles, so they pass on any
//! target — but they only cover the vector path when the test binary itself is
//! built for wasm32+simd128 and executed in a wasm runtime. That is what the
//! `rust-wasm-simd` CI lane does: it builds this crate's test target with
//! `RUSTFLAGS="-C target-feature=+simd128"` and runs it under node via
//! `wasm-bindgen-test-runner`.
//!
//! The test file refuses to compile for wasm32 without `+simd128`: a silent
//! scalar run would pretend to cover the vector path.
//!
//! Case selection mirrors the native differential tests
//! (`simd_path_agrees_with_scalar_oracle` in nntune-core's `yenc_vectors.rs`
//! and `kernel_matches_independent_russian_peasant_model` in its
//! `par2_create.rs`). The point is running the same coverage where the simd
//! dispatch is live, not inventing new vectors.
