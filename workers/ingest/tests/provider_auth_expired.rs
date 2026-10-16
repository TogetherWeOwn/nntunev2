//! S7 RED: indexer HTTP 401 on the ingest path must map to the documented
//! indexer-specific `indexer_auth_expired` code (never the NNTP
//! provider's `provider_auth_expired`, whose prompt asks for the wrong
//! secret), with no credential material.

use nntune_ingest::newznab::is_indexer_auth_failure;

#[test]
fn ingest_documents_indexer_auth_expired_code_distinct_from_provider() {
    assert_eq!(
        nntune_ingest::newznab::INDEXER_AUTH_EXPIRED_CODE,
        "indexer_auth_expired"
    );
    assert_ne!(
        nntune_ingest::newznab::INDEXER_AUTH_EXPIRED_CODE,
        "provider_auth_expired",
        "an expired indexer key must not trigger the provider-password prompt"
    );
}

#[test]
fn indexer_401_maps_to_indexer_code_but_403_does_not() {
    assert!(is_indexer_auth_failure(401));
    // 403 is routinely a rate limit / WAF / blocked account, not a bad key.
    assert!(!is_indexer_auth_failure(403));
    assert!(!is_indexer_auth_failure(500));
    assert!(!is_indexer_auth_failure(200));
}

#[test]
fn redacted_url_never_carries_the_key_on_auth_failure() {
    let bare = "https://indexer.example/api?t=music&o=json&q=x&limit=25&offset=0";
    let full = nntune_ingest::newznab::inject_api_key(bare, "test-api-key-1").unwrap();
    let redacted = nntune_ingest::newznab::redact_api_key(&full);
    assert!(!redacted.contains("test-api-key-1"));
    assert!(redacted.contains("apikey=***"));
}
