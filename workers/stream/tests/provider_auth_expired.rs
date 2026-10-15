//! S7 RED: forced NNTP 481/482 on the miss path must surface the
//! documented `provider_auth_expired` code as an inline re-auth signal,
//! never a quarantined outage and never credential material.

use nntune_stream::nntp::NntpError;
use nntune_stream::play::PlayError;

#[test]
fn stream_documents_provider_auth_expired_code() {
    assert_eq!(
        nntune_stream::nntp::PROVIDER_AUTH_EXPIRED_CODE,
        "provider_auth_expired"
    );
    assert_eq!(
        nntune_stream::play::PROVIDER_AUTH_EXPIRED_CODE,
        "provider_auth_expired"
    );
}

#[test]
fn nntp_auth_expired_is_not_a_quarantineable_provider_failure() {
    let err = NntpError::AuthExpired;
    assert!(
        !err.is_provider_failure(),
        "stale creds must prompt re-auth, not quarantine the pool"
    );
    assert_eq!(err.code(), "provider_auth_expired");
}

#[test]
fn play_error_carries_documented_code_without_secrets() {
    let secret = "s3cret-provider-password";
    let err = PlayError::ProviderAuthExpired;
    let rendered = format!("{err:?}");
    assert!(
        !rendered.contains(secret),
        "secret in PlayError: {rendered}"
    );
    assert_eq!(err.code(), "provider_auth_expired");
}
