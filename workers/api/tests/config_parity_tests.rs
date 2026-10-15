//! Acceptance tests for the staging/prod KV config parity check.
//!
//! Written first (TDD RED): these fail until `nntune_api::config` exists
//! with `check_parity`. This file is the fixture-driven CI-job test: the
//! parity job loads the staging vs prod key sets and fails on a type
//! mismatch or a missing required key, and passes on identical fixtures.
//! No live KV namespace is touched — both environments are fixture maps.
//! These test files are owned by the acceptance-test card: the paired
//! build card implements against them and never edits them.
//!
//! Contract pinned for the Builder (additive extensions allowed, the
//! signature below must hold):
//! - `nntune_api::config::check_parity(
//!     &schema, &staging: &HashMap<String, String>, &prod: &HashMap<String, String>
//!   ) -> Result<(), Vec<ParityFinding>>`
//!   validates each side against the schema with the typed-accessor
//!   rules (defaults fill absent optional keys; missing required keys
//!   and unparsable values become findings) and returns `Ok` only when
//!   both sides validate.
//! - `nntune_api::config::ParityFinding` implements `Debug` + `Display`,
//!   and its `Display` names the offending key, the environment
//!   (`staging` or `prod`), and the reason (missing required key vs
//!   type mismatch).

use nntune_api::config::{check_parity, ConfigSchema};
use std::collections::HashMap;

fn schema() -> ConfigSchema {
    let json = include_str!("fixtures/config-schema.json");
    ConfigSchema::from_json(json).expect("schema fixture parses")
}

fn kv_map(json: &str) -> HashMap<String, String> {
    serde_json::from_str(json).expect("kv fixture parses")
}

fn finding_text(findings: &[impl std::fmt::Display]) -> String {
    findings
        .iter()
        .map(|f| f.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn matching_fixtures_are_green() {
    let schema = schema();
    let staging = kv_map(include_str!("fixtures/config-staging.matching.json"));
    let prod = kv_map(include_str!("fixtures/config-prod.matching.json"));
    assert!(
        check_parity(&schema, &staging, &prod).is_ok(),
        "identical valid fixtures must pass the parity check"
    );
}

#[test]
fn defaults_only_maps_are_green() {
    // Both sides set only the required key; optional keys fall back to
    // schema defaults, so the parity check still passes.
    let schema = schema();
    let staging: HashMap<String, String> = [(
        "PUBLIC_BASE_URL".to_string(),
        "https://example.com".to_string(),
    )]
    .into_iter()
    .collect();
    let prod = staging.clone();
    assert!(
        check_parity(&schema, &staging, &prod).is_ok(),
        "defaults-only maps must pass the parity check"
    );
}

#[test]
fn drifted_fixtures_are_red_with_both_findings() {
    let schema = schema();
    // staging carries a type mismatch (STREAM_CACHE_TTL_SECS is not a
    // number); prod omits the required PUBLIC_BASE_URL.
    let staging = kv_map(include_str!("fixtures/config-staging.drifted.json"));
    let prod = kv_map(include_str!("fixtures/config-prod.drifted.json"));
    let findings = check_parity(&schema, &staging, &prod)
        .expect_err("drifted fixtures must fail the parity check");
    assert!(
        findings.len() >= 2,
        "both drift modes surface findings; got {}: {}",
        findings.len(),
        finding_text(&findings)
    );
    let text = finding_text(&findings);
    assert!(
        text.contains("STREAM_CACHE_TTL_SECS"),
        "type-mismatch key is reported: {text}"
    );
    assert!(
        text.contains("PUBLIC_BASE_URL"),
        "missing-required key is reported: {text}"
    );
    assert!(
        text.contains("staging") && text.contains("prod"),
        "findings name the failing environment: {text}"
    );
}
