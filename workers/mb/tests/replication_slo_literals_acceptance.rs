//! Pin replication SLO and breach-cap literals.
//!
//! The cap behavior is exercised symbolically by
//! `replication_cap_interaction.rs`; this suite pins the four literal
//! values themselves (`workers/mb/src/replication.rs:8,13,19,21`) so an
//! accidental retune turns this target red deliberately. Tests-only;
//! production code is unchanged.
//!
//! Sensitivity: each assertion is an exact-equality check on a distinct
//! constant. Mutating any production literal (e.g. `2 * 60 * 60` to
//! `3 * 60 * 60`, `10_000` to `1_000`, `100` to `50`, or the metric
//! string) breaks the matching assertion below. Verified 2026-10-18 by
//! running this target green against the current values, then
//! temporarily asserting a wrong value locally (red) before restoring.
//!
//! Standalone integration target: no existing suite is edited.

use nntune_mb::replication::{
    BREACH_METRIC_NAME, MAX_MISSING_SEQUENCE_BREACHES, MAX_STRUCTURED_LOG_BREACHES,
    REPLICATION_LAG_SLO_SECS,
};

#[test]
fn replication_lag_slo_is_two_hours() {
    assert_eq!(
        REPLICATION_LAG_SLO_SECS, 7_200,
        "ADR 0007 lag budget must stay at 2h (2 * 60 * 60 = 7_200s)",
    );
}

#[test]
fn missing_sequence_breach_cap_is_ten_thousand() {
    assert_eq!(
        MAX_MISSING_SEQUENCE_BREACHES, 10_000,
        "per-evaluation missing-sequence enumeration cap must stay at 10_000",
    );
}

#[test]
fn structured_log_breach_cap_is_one_hundred() {
    assert_eq!(
        MAX_STRUCTURED_LOG_BREACHES, 100,
        "structured-log breach array cap must stay at 100",
    );
}

#[test]
fn breach_metric_name_is_pinned() {
    assert_eq!(
        BREACH_METRIC_NAME, "mb_replication_slo_breach_total",
        "breach counter name is a cross-worker contract; renaming breaks dashboards",
    );
}
