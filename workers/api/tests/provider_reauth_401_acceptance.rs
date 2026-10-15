//! Acceptance tests: forced-401 maps to the documented
//! `provider_auth_expired` code (RED until the build lands).
//!
//! Test-only card: no `src/` changes here. These tests FAIL (compile error)
//! until the builder implements the contract below in
//! `workers/api/src/vault.rs` (pure core, no Worker runtime imports — the
//! HTTP wiring reuses these items verbatim). The builder MUST NOT edit this
//! file; implement `src/` to satisfy it.
//!
//! ## Builder contract (add to `vault.rs`)
//!
//! ```rust,ignore
//! /// Documented API error code for stale provider credentials. Shared
//! /// verbatim with the web app (`provider-reauth.ts`).
//! pub const PROVIDER_AUTH_EXPIRED_CODE: &str = "provider_auth_expired";
//!
//! /// True for NNTP reply codes that mean "re-authenticate": 481
//! /// (authentication rejected) and 482 (authentication required).
//! pub fn is_provider_auth_nntp_code(code: u16) -> bool;
//!
//! /// Redacted 401 payload for any provider-auth failure path
//! /// (NNTP 481/482, indexer HTTP 401). Serializable to the API error
//! /// body; `message` is generic and names re-entry, never secrets.
//! #[derive(Debug, Clone, serde::Serialize)]
//! pub struct ProviderAuthExpired {
//!     pub status: u16,        // always 401
//!     pub code: &'static str, // always PROVIDER_AUTH_EXPIRED_CODE
//!     pub message: &'static str,
//! }
//!
//! pub fn provider_auth_expired_response() -> ProviderAuthExpired;
//! ```
//!
//! Fixtures are synthetic only: forced NNTP reply codes and a synthetic
//! indexer 401 — no live provider traffic, no real secrets.

use nntune_api::vault;

#[test]
fn forced_401_surfaces_documented_provider_auth_expired_code() {
    // The documented code is pinned verbatim: the web re-auth prompt keys
    // off this exact string (see `provider-reauth.test.ts`).
    assert_eq!(vault::PROVIDER_AUTH_EXPIRED_CODE, "provider_auth_expired");

    // Forced-401 fixtures: NNTP rejects the stale credential (481/482) and
    // the indexer answers HTTP 401. Both are provider-auth failures.
    assert!(
        vault::is_provider_auth_nntp_code(481),
        "NNTP 481 (authentication rejected) must need re-auth"
    );
    assert!(
        vault::is_provider_auth_nntp_code(482),
        "NNTP 482 (authentication required) must need re-auth"
    );
    for code in [200u16, 211, 381, 430, 500, 502] {
        assert!(
            !vault::is_provider_auth_nntp_code(code),
            "NNTP {code} is not a credential failure"
        );
    }

    // Every provider-auth failure path returns the same redacted 401: the
    // documented code, never a generic error, never credential detail.
    let response = vault::provider_auth_expired_response();
    assert_eq!(
        response.status, 401,
        "stale credentials surface as HTTP 401"
    );
    assert_eq!(response.code, vault::PROVIDER_AUTH_EXPIRED_CODE);
    assert_eq!(response.code, "provider_auth_expired");
    assert!(
        !response.message.is_empty() && response.message.to_lowercase().contains("expir"),
        "message names expiry/re-entry so the UI can prompt: {}",
        response.message
    );
}

#[test]
fn provider_auth_expired_body_carries_no_credential_material() {
    // Negative test on the NEW response type: the 401 body must not carry
    // credential material. The redaction boundary matches the existing
    // `Debug` impls — password and indexer key never appear (host/user are
    // routing fields and already visible in `Debug`; secrets are not).
    const PASSWORD: &str = "synthetic-password-9f8e7d6c";
    const API_KEY: &str = "synthetic-indexer-key-1a2b3c4d";
    let _ = (PASSWORD, API_KEY); // documents what must never render

    let response = vault::provider_auth_expired_response();
    let body = serde_json::to_string(&response).expect("response serializes");
    assert!(
        body.contains(vault::PROVIDER_AUTH_EXPIRED_CODE),
        "body carries the documented code: {body}"
    );
    assert!(!body.contains(PASSWORD), "password in 401 body: {body}");
    assert!(!body.contains(API_KEY), "indexer key in 401 body: {body}");

    let dumped = format!("{response:?}");
    assert!(!dumped.contains(PASSWORD), "password in Debug: {dumped}");
    assert!(!dumped.contains(API_KEY), "indexer key in Debug: {dumped}");
}
