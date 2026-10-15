//! S7 RED: indexer/provider HTTP 401 on the ingest path must map to the
//! documented `provider_auth_expired` code with no credential material.

use nntune_ingest::newznab::is_provider_auth_failure;

#[test]
fn ingest_documents_provider_auth_expired_code() {
    assert_eq!(
        nntune_ingest::newznab::PROVIDER_AUTH_EXPIRED_CODE,
        "provider_auth_expired"
    );
}

#[test]
fn indexer_unauthorized_maps_to_reauth_code() {
    assert!(is_provider_auth_failure(401));
    assert!(is_provider_auth_failure(403));
    assert!(!is_provider_auth_failure(500));
    assert!(!is_provider_auth_failure(200));
}

#[test]
fn redacted_url_never_carries_the_key_on_auth_failure() {
    let bare = "https://indexer.example/api?t=music&o=json&q=x&limit=25&offset=0";
    let full = nntune_ingest::newznab::inject_api_key(bare, "test-api-key-1").unwrap();
    let redacted = nntune_ingest::newznab::redact_api_key(&full);
    assert!(!redacted.contains("test-api-key-1"));
    assert!(redacted.contains("provider_auth_expired") || redacted.contains("apikey=***"));
}
