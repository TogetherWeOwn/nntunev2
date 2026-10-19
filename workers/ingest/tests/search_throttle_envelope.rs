//! Search-throttle 429 as the standard error envelope (tests-only).
//!
//! Contract under test: a throttled Newznab search must answer 429 with
//! the worker's standard error envelope (`docs/api-errors.md`, rendered by
//! `workers/ingest/src/api_error.rs`):
//!
//! ```json
//! {"code": "rate_limited", "message": "too many requests", "retryable": true}
//! ```
//!
//! plus the `Retry-After` header, and zero identity material (no IP,
//! account, indexer key, or query) anywhere in the body.
//!
//! Defect pinned here: `search_rate_limited_body()`
//! (`workers/ingest/src/rate_limit.rs`) still returns the legacy shape
//! `{"error":"too many requests"}`, which carries neither `code` nor
//! `retryable`. The live route renders exactly that body
//! (`search_rate_limited_response` in `workers/ingest/src/lib.rs`), so the
//! paired fix is to make `search_rate_limited_body()` return the envelope
//! (e.g. `ApiError::rate_limited("too many requests").body_json()`) while
//! keeping the status, the `Retry-After` header, and the user-facing
//! wording unchanged.
//!
//! Builder note (pre-existing pin, not this file):
//! `rate_limited_body_is_generic_and_leaks_nothing` in
//! `tests/search_rate_limits_acceptance.rs` asserts the legacy
//! `parsed["error"]` shape. It predates this card and contradicts the
//! envelope contract, so the paired build must update that one assertion
//! to the envelope shape; its leak canaries stay valid. This file itself
//! is acceptance-owned: do not edit it to make the build pass.
//!
//! RED/GREEN expectation: on the pre-fix tree the envelope tests below
//! fail by assertion (missing `code`/`retryable`, legacy `error` key);
//! the status, `Retry-After`, determinism, and no-leak controls already
//! pass and must keep passing after the fix. Everything uses the
//! existing public API, so RED is an assertion failure, never a compile
//! error.
//!
//! Run only this file:
//!
//! ```sh
//! cd workers/ingest && cargo test --locked --test search_throttle_envelope
//! ```
//!
//! Canaries are synthetic (`203.0.113.9` TEST-NET-3, fake account/key/query
//! strings): no real IPs or credential material anywhere in this file.

use nntune_ingest::api_error::{spec, ApiError};
use nntune_ingest::rate_limit::{
    check_search_throttle, search_rate_limited_body, search_retry_after_header, SearchDecision,
    TokenBucket, RETRY_AFTER_HEADER, SEARCH_RATE_LIMIT_STATUS,
};

/// Fixed clock: bucket math is a pure function of `now_s`.
const NOW: u64 = 1_700_000_000;

/// The 429 body, parsed. Every assertion works on the parsed value so a
/// formatting-only difference (key order, spacing) cannot mask a shape
/// change or fake one.
fn parsed_body() -> serde_json::Value {
    let body = search_rate_limited_body();
    serde_json::from_str(&body).expect("throttle 429 body must be JSON")
}

// --- The envelope itself (RED before the paired fix) -------------------------

#[test]
fn body_is_the_rate_limited_error_envelope() {
    let parsed = parsed_body();
    assert_eq!(
        parsed["code"], "rate_limited",
        "429 body must carry the registry code, got: {parsed}"
    );
    assert_eq!(
        parsed["retryable"], true,
        "a throttle denial is retryable once Retry-After elapses, got: {parsed}"
    );
    assert_eq!(
        parsed["message"], "too many requests",
        "the envelope change must keep the existing user-facing wording, got: {parsed}"
    );
    assert!(
        parsed.get("error").is_none(),
        "the legacy {{\"error\": ...}} shape must be gone, got: {parsed}"
    );
}

#[test]
fn body_envelope_has_exactly_code_message_retryable_keys() {
    let parsed = parsed_body();
    let object = parsed
        .as_object()
        .unwrap_or_else(|| panic!("429 body must be a JSON object, got: {parsed}"));
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["code", "message", "retryable"],
        "envelope is exactly {{code, message, retryable}}: no extra field \
         can smuggle identity material into the 429 body"
    );
}

#[test]
fn body_matches_the_canonical_api_error_envelope() {
    // The throttle body must be the same envelope every other ingest
    // error renders, not a second hand-rolled shape that can drift.
    let canonical = ApiError::rate_limited("too many requests");
    assert_eq!((canonical.status, canonical.retryable), (429, true));
    let canonical_body: serde_json::Value =
        serde_json::from_str(&canonical.body_json()).expect("ApiError body must be JSON");
    assert_eq!(
        parsed_body(),
        canonical_body,
        "search-throttle 429 body must equal ApiError::rate_limited's envelope"
    );
}

// --- Status + Retry-After preserved (controls) -------------------------------

#[test]
fn throttle_status_agrees_with_the_error_registry() {
    let registry = spec("rate_limited");
    assert_eq!((registry.status, registry.retryable), (429, true));
    assert_eq!(
        SEARCH_RATE_LIMIT_STATUS, registry.status,
        "throttle status and the rate_limited registry row must not drift apart"
    );
}

#[test]
fn retry_after_is_preserved_on_a_real_denial() {
    assert_eq!(RETRY_AFTER_HEADER, "retry-after");

    // Drive the actual dual gate to a denial, then render the header
    // exactly as the route does.
    let mut ip = TokenBucket::per_window(1, 60, NOW);
    let mut account = TokenBucket::per_window(1, 60, NOW);
    assert_eq!(
        check_search_throttle(&mut ip, &mut account, NOW),
        SearchDecision::Allow
    );
    let retry_after_secs = match check_search_throttle(&mut ip, &mut account, NOW) {
        SearchDecision::Deny { retry_after_secs } => retry_after_secs,
        other => panic!("spent buckets must deny, got: {other:?}"),
    };
    assert!(
        retry_after_secs >= 1,
        "a denial must carry a positive Retry-After, got {retry_after_secs}"
    );
    let header = search_retry_after_header(retry_after_secs);
    assert_eq!(header.parse::<u64>().ok(), Some(retry_after_secs));
    assert_eq!(search_retry_after_header(42), "42");
}

// --- Zero identity material (controls) ----------------------------------------

#[test]
fn body_carries_zero_identity_material() {
    // Zero-arg by construction: the vault-resolved indexer key, the query,
    // and the caller identity cannot flow into this body. These canaries
    // pin the rendered text, not just the signature.
    let body = search_rate_limited_body();
    for canary in [
        "203.0.113.9",
        "113.9",
        "acct-canary-1",
        "test-api-key-1",
        "super-secret-key",
        "apikey",
        "dark side moon",
    ] {
        assert!(
            !body.contains(canary),
            "429 body must not echo per-request data ({canary}): {body}"
        );
    }
}

#[test]
fn body_is_constant_across_calls() {
    // A generic body cannot vary per caller: two renders are identical,
    // so nothing about the request can be encoded in it.
    assert_eq!(search_rate_limited_body(), search_rate_limited_body());
}
