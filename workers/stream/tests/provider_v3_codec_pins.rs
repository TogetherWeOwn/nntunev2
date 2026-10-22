//! Builder-owned pins for the inert provider-v3 codec (review follow-up).
//!
//! The QA-owned contract suites (`provider_v3_response_acceptance`,
//! `provider_v3_inert_guard_acceptance`) stay untouched. These pins lock the
//! exact accept-path escape decoding and the documented denial-code mapping
//! that review probed correct, so future edits cannot silently change them.
//! Parsing is NOT authorization: no runtime wiring, no authority handle.

use nntune_stream::provider_response::{
    validate_provider_response, ExpectedProviderResponse, KeyBinding, ProviderResponseError,
};
use serde_json::{json, Value};

const OWNER_WIRE: &str = "\"inert\\\\owner_\\ud83d\\ude00\"";
const OWNER_DECODED: &str = "inert\\owner_\u{1f600}";
const PASSWORD_WIRE: &str = "\"a\\\\b\\\"c\\ud83d\\ude00\"";
const PASSWORD_DECODED: &str = "a\\b\"c\u{1f600}";
const USERNAME_WIRE: &str = "\"a\\/b\"";

const REQUEST: &str = "00000000-0000-4000-8000-000000000001";
const LEASE: &str = "00000000-0000-4000-8000-000000000002";
const PROVIDER: &str = "00000000-0000-4000-8000-000000000003";
const SCOPE: &str = "00000000-0000-4000-8000-000000000004";
const SCOPE_DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CONTEXT_DIGEST: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const PLAN_DIGEST: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

// Shape-only invented IDs/digests/key labels; no keys, tokens, or signatures.
fn expected() -> ExpectedProviderResponse {
    ExpectedProviderResponse {
        request_id: REQUEST.into(),
        lease_id: LEASE.into(),
        lease_until: 1_000_015,
        pool_owner: OWNER_DECODED.into(),
        provider_id: PROVIDER.into(),
        provider_version: 1,
        owner_epoch: 2,
        permission_epoch: 3,
        scope_id: SCOPE.into(),
        operation: "nntp.body".into(),
        scope_digest: SCOPE_DIGEST.into(),
        context_digest: CONTEXT_DIGEST.into(),
        plan_digest: PLAN_DIGEST.into(),
        cnf: KeyBinding {
            kid: "inert_stream".into(),
            kv: 4,
        },
        issuer_key: KeyBinding {
            kid: "inert_issuer".into(),
            kv: 5,
        },
    }
}

fn positive() -> Value {
    json!({
        "v":3,"request_id":REQUEST,"lease_id":LEASE,"lease_until":1_000_015,
        "pool_owner":"POOL_OWNER_SENTINEL","provider_id":PROVIDER,"provider_version":1,
        "owner_epoch":2,"permission_epoch":3,"scope_id":SCOPE,"operation":"nntp.body",
        "provider":{"host":"news.example.invalid","port":563,"tls":true,
            "allow_insecure_plaintext":false,"username":"USERNAME_SENTINEL","password":"PASSWORD_SENTINEL",
            "effective_max_connections":1},
        "scope_digest":SCOPE_DIGEST,"context_digest":CONTEXT_DIGEST,"plan_digest":PLAN_DIGEST,
        "cnf":{"kid":"inert_stream","kv":4},"issuer_key":{"kid":"inert_issuer","kv":5}
    })
}

/// Serialize the valid shape, then swap sentinel values for raw wire-escaped
/// fragments so the surrogate-pair and backslash arms are exercised exactly.
fn escaped_wire() -> Vec<u8> {
    let text = serde_json::to_string(&positive()).expect("serialize pin input");
    for sentinel in [
        "POOL_OWNER_SENTINEL",
        "USERNAME_SENTINEL",
        "PASSWORD_SENTINEL",
    ] {
        assert_eq!(
            text.matches(sentinel).count(),
            1,
            "sentinel must occur exactly once"
        );
    }
    text.replace("\"POOL_OWNER_SENTINEL\"", OWNER_WIRE)
        .replace("\"USERNAME_SENTINEL\"", USERNAME_WIRE)
        .replace("\"PASSWORD_SENTINEL\"", PASSWORD_WIRE)
        .into_bytes()
}

#[test]
fn accepted_escapes_decode_to_exact_provider_bytes() {
    let binding = expected();
    let parsed =
        validate_provider_response(&escaped_wire(), &binding).expect("escaped inert shape denied");
    assert_eq!(parsed.bindings(), &binding);
    assert_eq!(parsed.bindings().pool_owner, OWNER_DECODED);
    assert_eq!(parsed.provider().username(), "a/b");
    assert_eq!(parsed.provider().password(), PASSWORD_DECODED);
}

fn deny_exact(raw: &[u8], binding: &ExpectedProviderResponse) -> ProviderResponseError {
    match validate_provider_response(raw, binding) {
        Ok(_) => panic!("invalid inert shape accepted"),
        Err(error) => {
            assert!(
                matches!(
                    error,
                    ProviderResponseError::AuthorizationDenied
                        | ProviderResponseError::ProviderUnavailable
                ),
                "denial must be a fixed sanitized code"
            );
            error
        }
    }
}

#[test]
fn binding_mismatch_denies_with_authorization_denied_while_malformed_is_unavailable() {
    let wire = escaped_wire();
    // Well-formed wire, mismatched supplied bindings: numeric and string arms.
    let mut numeric = expected();
    numeric.lease_until = 1_000_016;
    let error = deny_exact(&wire, &numeric);
    assert_eq!(error, ProviderResponseError::AuthorizationDenied);
    assert_eq!(error.to_string(), "authorization_denied");
    let mut digest = expected();
    digest.scope_digest = format!("0{}", &SCOPE_DIGEST[1..]);
    let error = deny_exact(&wire, &digest);
    assert_eq!(error, ProviderResponseError::AuthorizationDenied);

    // Unusable responses deny as unavailable, never as a binding mismatch.
    let valid = serde_json::to_string(&positive()).expect("serialize pin input");
    let wrong_version = valid.replacen("\"v\":3", "\"v\":2", 1).into_bytes();
    let error = deny_exact(&wrong_version, &expected());
    assert_eq!(error, ProviderResponseError::ProviderUnavailable);
    assert_eq!(error.to_string(), "provider_unavailable");
    let truncated = &serde_json::to_vec(&positive()).expect("serialize pin input")[..100];
    assert_eq!(
        deny_exact(truncated, &expected()),
        ProviderResponseError::ProviderUnavailable
    );
    let mut oversize = serde_json::to_vec(&positive()).expect("serialize pin input");
    oversize.resize(8 * 1024 + 1, b' ');
    assert_eq!(
        deny_exact(&oversize, &expected()),
        ProviderResponseError::ProviderUnavailable
    );
}
