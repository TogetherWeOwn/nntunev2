//! Acceptance coverage for the ingest Worker's independent liveness endpoint.

const SOURCE: &str = include_str!("../src/lib.rs");

// Worker Response conversion is wasm-only; these native acceptance checks pin
// the response builder and fetch-router contract without requiring bindings.

fn section_after<'a>(source: &'a str, marker: &str) -> &'a str {
    source
        .split_once(marker)
        .unwrap_or_else(|| panic!("missing source contract: {marker}"))
        .1
        .split_once("\n}\n")
        .unwrap_or_else(|| panic!("unterminated source contract: {marker}"))
        .0
}

#[test]
fn healthz_response_builder_matches_the_liveness_contract() {
    let response = section_after(SOURCE, "fn health_response()");

    assert!(
        response.contains("http::StatusCode::OK"),
        "liveness must return 200"
    );
    assert!(response.contains("http::header::CONTENT_TYPE"));
    assert!(response.contains("\"application/json\""));
    assert!(response.contains("http::header::CACHE_CONTROL"));
    assert!(response.contains("\"no-store\""));
    assert!(response.contains(r##"r#"{"ok":true}"#"##));

    let handler = section_after(SOURCE, "fn healthz() -> Result<Response> {");
    assert!(handler.contains("health_response_parts()"));
}

#[test]
fn healthz_is_a_get_route_before_the_not_found_catchall() {
    let entrypoint = SOURCE
        .split_once("pub async fn main(")
        .expect("Worker fetch entrypoint must exist")
        .1;
    let router = entrypoint
        .split_once("\n#[cfg(test)]\nmod tests")
        .map_or(entrypoint, |(router, _)| router);
    let health_route = router
        .find(".get_async(\"/healthz\"")
        .expect("GET /healthz must be registered");
    let catchall = router
        .find(".or_else_any_method_async")
        .expect("router must retain its not-found fallback");
    assert!(
        health_route < catchall,
        "healthz must precede the catch-all"
    );
    assert!(router[health_route..catchall].contains("healthz()"));
    assert_eq!(router.matches(".get_async(\"/healthz\"").count(), 1);
}
