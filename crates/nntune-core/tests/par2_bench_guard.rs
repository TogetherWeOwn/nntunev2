//! Acceptance tests for [TOG-25180](/TOG/issues/TOG-25180) (roadmap R5 from
//! [TOG-25148](/TOG/issues/TOG-25148)): par2 verify bench guard.
//!
//! Builder contract (implement in
//! `crates/nntune-core/src/par2/bench.rs`, exposed as
//! `pub mod bench` in `crates/nntune-core/src/par2.rs`):
//!
//! ```rust,ignore
//! /// Pinned baseline, ms per MiB of verified payload, measured on the
//! /// reference machine and recorded by the Builder (same reporting style as
//! /// `crates/nntune-core/benches/yenc.rs`: ms/MB).
//! pub const VERIFY_BASELINE_MS_PER_MB: f64;
//! /// Maximum allowed regression: measurements above
//! /// `baseline * MAX_REGRESSION_RATIO` fail the guard.
//! pub const MAX_REGRESSION_RATIO: f64; // 1.20
//!
//! /// Verifies a synthetic 1 MiB payload against an in-memory recovery set
//! /// and returns the measured throughput in ms per MiB. Deterministic
//! /// input (fixed seed), no I/O, no subprocess: safe to run in unit CI.
//! pub fn verify_throughput_ms_per_mb() -> f64;
//! ```
//!
//! The guard test (`verify_throughput_stays_within_regression_bound`) fails
//! when par2 verification regresses by more than 20% versus the pinned
//! baseline — the same policy as the yEnc bench gate on the build card.
//!
//! Failing command:
//! `cargo test -p nntune-core --test par2_bench_guard`
//! (also runs under `cargo test -p nntune-core par2`)
//!
//! RED reason (2026-10-15): `nntune_core::par2::bench` does not exist yet,
//! so this target fails to compile with `unresolved import` (E0432). The
//! Builder on [TOG-25180](/TOG/issues/TOG-25180) turns it green without
//! editing this file. Synthetic fixtures only, no provider.

use nntune_core::par2::bench::{
    verify_throughput_ms_per_mb, MAX_REGRESSION_RATIO, VERIFY_BASELINE_MS_PER_MB,
};

#[test]
fn regression_bound_is_twenty_percent() {
    assert!(
        (MAX_REGRESSION_RATIO - 1.20).abs() < f64::EPSILON,
        "bench guard must fail on >20% regression (ratio 1.20), got {MAX_REGRESSION_RATIO}"
    );
}

#[test]
fn baseline_is_a_positive_finite_reference() {
    assert!(
        VERIFY_BASELINE_MS_PER_MB.is_finite() && VERIFY_BASELINE_MS_PER_MB > 0.0,
        "baseline must be a positive finite ms/MiB reference, got {}",
        VERIFY_BASELINE_MS_PER_MB
    );
    // Sanity: a verify baseline in the hours-per-MiB range means the unit is
    // wrong (seconds vs ms) — catch it here, not in CI triage.
    assert!(
        VERIFY_BASELINE_MS_PER_MB < 60_000.0,
        "baseline {} ms/MiB is implausibly slow; check the unit (want ms/MiB)",
        VERIFY_BASELINE_MS_PER_MB
    );
}

#[test]
fn verify_throughput_stays_within_regression_bound() {
    let measured = verify_throughput_ms_per_mb();
    assert!(
        measured.is_finite() && measured >= 0.0,
        "measurement must be a finite non-negative ms/MiB value, got {measured}"
    );
    let ceiling = VERIFY_BASELINE_MS_PER_MB * MAX_REGRESSION_RATIO;
    assert!(
        measured <= ceiling,
        "par2 verify regressed >20%: measured {measured:.3} ms/MiB vs \
         baseline {VERIFY_BASELINE_MS_PER_MB:.3} ms/MiB (ceiling {ceiling:.3})"
    );
}
