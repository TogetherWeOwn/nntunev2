//! Independently owned live-loader error-mapping acceptance tests.
//! Oracle: pinned seam S:356 (exact `{v:3,request_id,error:Code}`; 403
//! authorization_denied; 409 provider_not_configured only after full
//! authorization; 503 provider_unavailable; public/unrecognized 404
//! not_found; fresh unrelated UUID for invalid IDs; no existence
//! disclosure, secret/body/hash/snippet/debug errors or key retry). See
//! loader-slice-acceptance.md for the oracle, seam, matrix and proof limits.
//! This handoff is intentionally RED until the portable seam exists.
//! IDs are inert synthetic UUIDs; no vault, DB, clock or network.

use nntune_stream::loader::{map_loader_error, LoaderDenial, MappedLoaderError};

const REQUEST: &str = "123e4567-e89b-42d3-a456-426614174000";
const FRESH: &str = "123e4567-e89b-42d3-a456-426614174999";
const OTHER_FRESH: &str = "123e4567-e89b-42d3-a456-426614174001";

fn map(denial: LoaderDenial, request_id: &str, authorized: bool) -> MappedLoaderError {
    map_loader_error(denial, request_id, FRESH, authorized)
}

fn exact(id: &str, code: &str) -> String {
    format!("{{\"v\":3,\"request_id\":\"{id}\",\"error\":\"{code}\"}}")
}

#[test]
fn denial_code_and_status_table_is_exact() {
    for (denial, code, status) in [
        (
            LoaderDenial::AuthorizationDenied,
            "authorization_denied",
            403,
        ),
        (
            LoaderDenial::ProviderNotConfigured,
            "provider_not_configured",
            409,
        ),
        (
            LoaderDenial::ProviderUnavailable,
            "provider_unavailable",
            503,
        ),
        (LoaderDenial::NotFound, "not_found", 404),
    ] {
        assert_eq!(denial.code(), code);
        assert_eq!(denial.status(), status);
        assert_eq!(denial.to_string(), code);
    }
}

#[test]
fn authorization_denied_maps_to_exact_403_wire_shape() {
    let mapped = map(LoaderDenial::AuthorizationDenied, REQUEST, false);
    assert_eq!(mapped.status(), 403);
    assert_eq!(mapped.body(), exact(REQUEST, "authorization_denied"));
}

#[test]
fn provider_not_configured_maps_to_409_only_after_full_authorization() {
    let authorized = map(LoaderDenial::ProviderNotConfigured, REQUEST, true);
    assert_eq!(authorized.status(), 409);
    assert_eq!(authorized.body(), exact(REQUEST, "provider_not_configured"));
    // Without full authorization the same condition must not disclose
    // configuration existence: it maps to 403 instead.
    let denied = map(LoaderDenial::ProviderNotConfigured, REQUEST, false);
    assert_eq!(denied.status(), 403);
    assert_eq!(denied.body(), exact(REQUEST, "authorization_denied"));
}

#[test]
fn authorization_gate_applies_to_unavailable_as_well() {
    let authorized = map(LoaderDenial::ProviderUnavailable, REQUEST, true);
    assert_eq!(authorized.status(), 503);
    assert_eq!(authorized.body(), exact(REQUEST, "provider_unavailable"));
    let denied = map(LoaderDenial::ProviderUnavailable, REQUEST, false);
    assert_eq!(denied.status(), 403);
    assert_eq!(denied.body(), exact(REQUEST, "authorization_denied"));
}

#[test]
fn public_or_unrecognized_routes_map_to_404_regardless_of_authorization() {
    for authorized in [false, true] {
        let mapped = map(LoaderDenial::NotFound, REQUEST, authorized);
        assert_eq!(mapped.status(), 404);
        assert_eq!(mapped.body(), exact(REQUEST, "not_found"));
    }
}

#[test]
fn valid_request_id_echoes_byte_exact() {
    let mapped = map(LoaderDenial::ProviderUnavailable, REQUEST, true);
    assert_eq!(mapped.body(), exact(REQUEST, "provider_unavailable"));
    assert!(!mapped.body().contains(FRESH));
}

#[test]
fn invalid_request_ids_yield_the_fresh_unrelated_uuid() {
    let upper = REQUEST.to_ascii_uppercase();
    let long = "a".repeat(500);
    for invalid in [
        String::new(),
        "not-a-uuid".to_string(),
        upper,
        // Version 1, not version 4.
        "123e4567-e89b-12d3-a456-426614174000".to_string(),
        // Nil UUID: version nibble 0, not 4.
        "00000000-0000-0000-0000-000000000000".to_string(),
        "123e4567e89b42d3a456426614174000".to_string(),
        "{123e4567-e89b-42d3-a456-426614174000}".to_string(),
        format!(" {REQUEST}"),
        long,
    ] {
        let mapped = map(LoaderDenial::ProviderUnavailable, &invalid, true);
        assert_eq!(mapped.status(), 503);
        assert_eq!(mapped.body(), exact(FRESH, "provider_unavailable"));
        assert_ne!(FRESH, invalid.as_str());
    }
}

#[test]
fn credential_bearing_ids_never_echo_and_compare_equal() {
    // Distinct invalid secrets map to identical fixed bodies: nothing
    // caller-supplied survives into the response.
    let first = "password=hunter2-a&request_id=x";
    let second = "password=hunter2-b&request_id=y";
    let one = map(LoaderDenial::ProviderUnavailable, first, true);
    let two = map(LoaderDenial::ProviderUnavailable, second, true);
    assert_eq!(one.body(), two.body());
    assert_eq!(one.body(), exact(FRESH, "provider_unavailable"));
    assert!(!one.body().contains("hunter2"));
    assert!(!one.body().contains("password"));
}

#[test]
fn fresh_substitution_uses_the_supplied_authority_uuid_verbatim() {
    let mapped = map_loader_error(
        LoaderDenial::AuthorizationDenied,
        "bogus",
        OTHER_FRESH,
        false,
    );
    assert_eq!(mapped.status(), 403);
    assert_eq!(mapped.body(), exact(OTHER_FRESH, "authorization_denied"));
}

#[test]
fn mapped_bodies_are_closed_three_key_objects() {
    // Exact shape, key order and scalar forms: integer 3 (not a string),
    // lowercase UUID text, fixed code text. Parsed back, the object holds
    // exactly v/request_id/error.
    let mapped = map(LoaderDenial::ProviderUnavailable, REQUEST, true);
    let body = mapped.body();
    assert!(body.starts_with("{\"v\":3,\"request_id\":\""));
    assert!(body.ends_with("\",\"error\":\"provider_unavailable\"}"));
    let parsed: serde_json::Value = serde_json::from_str(body).expect("mapped body parses as JSON");
    let object = parsed.as_object().expect("mapped body is an object");
    assert_eq!(object.len(), 3);
    assert_eq!(object["v"], serde_json::Value::from(3));
    assert_eq!(object["request_id"], serde_json::Value::from(REQUEST));
    assert_eq!(
        object["error"],
        serde_json::Value::from("provider_unavailable")
    );
}
