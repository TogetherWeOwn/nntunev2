//! Native acceptance for the DB error policy shared by ingest writers.
//!
//! These tests exercise the production classifier, not a persistence model.
//! They do not prove admission, publication, terminal settlement, SQL binds,
//! or deletion-lock execution; those need a production-used DB executor seam.

use nntune_ingest::library::is_deterministic_sqlstate;

#[test]
fn missing_schema_is_a_deterministic_writer_failure() {
    // PostgreSQL invalid_schema_name: replaying the same statement cannot
    // create its missing schema. Treat it like undefined_table (42P01),
    // rather than spending the job's bounded redelivery budget.
    assert!(
        is_deterministic_sqlstate(Some("3F000")),
        "3F000 (invalid_schema_name) must fail permanently, not enter transient redelivery"
    );
}

#[test]
fn missing_relation_and_constraint_failures_remain_deterministic() {
    for sqlstate in ["42P01", "42703", "23503", "23505", "23514", "22003"] {
        assert!(
            is_deterministic_sqlstate(Some(sqlstate)),
            "{sqlstate} must remain a deterministic writer failure"
        );
    }
}

#[test]
fn recoverable_database_failures_keep_the_transient_retry_path() {
    // Connection refusal/loss, serialization/deadlock, resource pressure,
    // startup and a driver failure without SQLSTATE can recover. In
    // particular, adding missing-schema handling must not catch class 40.
    for sqlstate in [
        Some("08001"),
        Some("08006"),
        Some("40001"),
        Some("40P01"),
        Some("53300"),
        Some("57P03"),
        None,
    ] {
        assert!(
            !is_deterministic_sqlstate(sqlstate),
            "{sqlstate:?} must stay eligible for the existing bounded retry policy"
        );
    }
}
