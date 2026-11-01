//! Caller authentication for every `/v1/ingest/*` route.
//!
//! The ingest Worker serves no end users directly: the `api` Worker
//! authenticates the session and mints a short-lived caller grant scoped
//! [`INGEST_WRITE_SCOPE`], sent as `Authorization: Bearer <grant>`. The
//! grant's signed `user_id` is the only identity a route acts on — a body
//! `user_id` is never trusted (see [`bind_body_user`]).
//!
//! Token shape: `v1.<hex(payload)>.<hex(hmac-sha256(payload_hex))>` where the
//! payload is `user_id|scope|exp_unix`, signed with the shared
//! [`CALLER_SECRET_VAR`] Worker secret. Same scheme and lifetime caps as the
//! stream Worker's play grants (`workers/stream/src/grant.rs`), hex instead
//! of base64url so this crate needs no new dependency.
//!
//! Every rejection maps to one generic 401 (`unauthorized`) that never says
//! which check failed, and every route runs [`authenticate`] before reading
//! the body or touching the queue, KV, rate limiter, ledger or meter.

use sha2::{Digest, Sha256};

use crate::dispatch::validate_user_id;

/// Worker secret holding the HMAC key shared with the `api` Worker
/// (`wrangler secret put INGEST_CALLER_SECRET`; never a `[vars]` entry).
pub const CALLER_SECRET_VAR: &str = "INGEST_CALLER_SECRET";

/// Minimum accepted secret length in bytes: a shorter (or absent) secret is
/// a misconfiguration and fails closed for every caller.
pub const MIN_SECRET_BYTES: usize = 32;

/// The only scope the ingest routes accept.
pub const INGEST_WRITE_SCOPE: &str = "ingest:write";

/// Maximum grant lifetime: 15 minutes, matching the stream play grant.
pub const MAX_GRANT_TTL: u64 = 900;

/// Issuer clock skew tolerated on top of [`MAX_GRANT_TTL`].
pub const CLOCK_SKEW_SECS: u64 = 60;

/// The authenticated caller a route acts for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    pub user_id: String,
}

/// Why a request was refused. All variants render the same 401; the
/// distinction exists for tests only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthError {
    /// No `Authorization: Bearer` header.
    Missing,
    Malformed,
    BadSignature,
    WrongScope,
    Expired,
    TtlTooLong,
    /// The Worker secret is absent or shorter than [`MIN_SECRET_BYTES`].
    NotConfigured,
}

/// Mint a caller grant. The production issuer lives in the `api` Worker;
/// this byte-exact signer exists for tests and that issuer's vectors.
pub fn issue(user_id: &str, scope: &str, exp_unix: u64, secret: &[u8]) -> String {
    let payload_hex = hex::encode(format!("{user_id}|{scope}|{exp_unix}"));
    let sig = hmac_sha256(secret, payload_hex.as_bytes());
    format!("v1.{payload_hex}.{}", hex::encode(sig))
}

/// Authenticate a request from its `Authorization` header value and the
/// configured secret at `now_unix`.
pub fn authenticate(
    authorization: Option<&str>,
    secret: Option<&str>,
    now_unix: u64,
) -> Result<Caller, AuthError> {
    let secret = match secret {
        Some(s) if s.len() >= MIN_SECRET_BYTES => s.as_bytes(),
        _ => return Err(AuthError::NotConfigured),
    };
    let token = authorization
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(AuthError::Missing)?;
    verify(token, secret, now_unix)
}

/// Verify one grant token.
pub fn verify(token: &str, secret: &[u8], now_unix: u64) -> Result<Caller, AuthError> {
    let rest = token.strip_prefix("v1.").ok_or(AuthError::Malformed)?;
    let (payload_hex, sig_hex) = rest.split_once('.').ok_or(AuthError::Malformed)?;
    if payload_hex.is_empty() {
        return Err(AuthError::Malformed);
    }
    let sig = hex::decode(sig_hex).map_err(|_| AuthError::Malformed)?;
    let expected = hmac_sha256(secret, payload_hex.as_bytes());
    if sig.len() != expected.len() || !constant_time_eq(&sig, &expected) {
        return Err(AuthError::BadSignature);
    }
    let payload = hex::decode(payload_hex).map_err(|_| AuthError::Malformed)?;
    let payload = String::from_utf8(payload).map_err(|_| AuthError::Malformed)?;
    let mut parts = payload.split('|');
    let (user_id, scope, exp) = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(u), Some(s), Some(e), None) => (u, s, e),
        _ => return Err(AuthError::Malformed),
    };
    if !validate_user_id(user_id) {
        return Err(AuthError::Malformed);
    }
    if scope != INGEST_WRITE_SCOPE {
        return Err(AuthError::WrongScope);
    }
    let exp_unix: u64 = exp.parse().map_err(|_| AuthError::Malformed)?;
    if now_unix >= exp_unix {
        return Err(AuthError::Expired);
    }
    // `now_unix < exp_unix` here, so the subtraction cannot underflow.
    if exp_unix - now_unix > MAX_GRANT_TTL + CLOCK_SKEW_SECS {
        return Err(AuthError::TtlTooLong);
    }
    Ok(Caller {
        user_id: user_id.to_string(),
    })
}

/// Bind a request body's optional `user_id` to the authenticated caller.
/// Absent is fine (the caller's id is used); a present value that differs
/// is a spoof attempt and is refused before anything is built or queued.
pub fn bind_body_user(caller: &Caller, body_user_id: Option<&str>) -> Result<(), &'static str> {
    match body_user_id {
        Some(claimed) if claimed != caller.user_id => {
            Err("user_id does not match the authenticated caller")
        }
        _ => Ok(()),
    }
}

/// Job ownership: a checkpoint is visible only to the user it belongs to.
pub fn owns_job(caller: &Caller, job_user_id: &str) -> bool {
    constant_time_eq_str(&caller.user_id, job_user_id)
}

fn constant_time_eq_str(a: &str, b: &str) -> bool {
    a.len() == b.len() && constant_time_eq(a.as_bytes(), b.as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// HMAC-SHA256 (RFC 2104) over the crate's existing `sha2` dependency.
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut key_block = [0u8; 64];
    if key.len() > 64 {
        key_block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(key_block.map(|k| k ^ 0x36));
    inner.update(msg);
    let mut outer = Sha256::new();
    outer.update(key_block.map(|k| k ^ 0x5c));
    outer.update(inner.finalize());
    let mut out = [0u8; 32];
    out.copy_from_slice(&outer.finalize());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "test-secret-test-secret-test-secret!";
    const NOW: u64 = 1_800_000_000;

    fn bearer(user_id: &str, scope: &str, exp: u64) -> String {
        format!("Bearer {}", issue(user_id, scope, exp, SECRET.as_bytes()))
    }

    #[test]
    fn hmac_matches_rfc_4231_test_case_1() {
        assert_eq!(
            hex::encode(hmac_sha256(&[0x0bu8; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn hmac_matches_rfc_4231_test_case_6_long_key() {
        assert_eq!(
            hex::encode(hmac_sha256(
                &[0xaau8; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn valid_grant_yields_its_signed_user() {
        let header = bearer("user_01", INGEST_WRITE_SCOPE, NOW + 600);
        assert_eq!(
            authenticate(Some(&header), Some(SECRET), NOW),
            Ok(Caller {
                user_id: "user_01".to_string()
            })
        );
    }

    #[test]
    fn unauthenticated_and_forged_requests_are_refused() {
        let ok = issue("user_01", INGEST_WRITE_SCOPE, NOW + 600, SECRET.as_bytes());
        let forged = issue(
            "user_01",
            INGEST_WRITE_SCOPE,
            NOW + 600,
            b"another-secret-another-secret-12",
        );
        let cases: [(Option<String>, AuthError); 8] = [
            (None, AuthError::Missing),
            (Some(String::new()), AuthError::Missing),
            (Some(ok.clone()), AuthError::Missing),
            (Some(format!("Basic {ok}")), AuthError::Missing),
            (Some("Bearer ".to_string()), AuthError::Malformed),
            (Some("Bearer v1.zz.zz".to_string()), AuthError::Malformed),
            (Some(format!("Bearer {forged}")), AuthError::BadSignature),
            (
                Some(format!("Bearer {}x", &ok[..ok.len() - 1])),
                AuthError::Malformed,
            ),
        ];
        for (header, want) in cases {
            assert_eq!(
                authenticate(header.as_deref(), Some(SECRET), NOW),
                Err(want),
                "{header:?}"
            );
        }
    }

    #[test]
    fn tampered_payload_fails_the_signature() {
        let token = issue("user_01", INGEST_WRITE_SCOPE, NOW + 600, SECRET.as_bytes());
        let (_, sig) = token.rsplit_once('.').unwrap();
        let swapped = format!(
            "Bearer v1.{}.{sig}",
            hex::encode(format!("user_02|{INGEST_WRITE_SCOPE}|{}", NOW + 600))
        );
        assert_eq!(
            authenticate(Some(&swapped), Some(SECRET), NOW),
            Err(AuthError::BadSignature)
        );
    }

    #[test]
    fn scope_expiry_and_lifetime_are_enforced() {
        let cases = [
            (
                bearer("user_01", "ingest:read", NOW + 600),
                AuthError::WrongScope,
            ),
            (
                bearer("user_01", INGEST_WRITE_SCOPE, NOW),
                AuthError::Expired,
            ),
            (
                bearer(
                    "user_01",
                    INGEST_WRITE_SCOPE,
                    NOW + MAX_GRANT_TTL + CLOCK_SKEW_SECS + 1,
                ),
                AuthError::TtlTooLong,
            ),
            (
                bearer("user:01", INGEST_WRITE_SCOPE, NOW + 600),
                AuthError::Malformed,
            ),
            (
                bearer("", INGEST_WRITE_SCOPE, NOW + 600),
                AuthError::Malformed,
            ),
        ];
        for (header, want) in cases {
            assert_eq!(authenticate(Some(&header), Some(SECRET), NOW), Err(want));
        }
        let at_cap = bearer(
            "user_01",
            INGEST_WRITE_SCOPE,
            NOW + MAX_GRANT_TTL + CLOCK_SKEW_SECS,
        );
        assert!(authenticate(Some(&at_cap), Some(SECRET), NOW).is_ok());
    }

    #[test]
    fn missing_or_short_secret_fails_closed() {
        let header = bearer("user_01", INGEST_WRITE_SCOPE, NOW + 600);
        assert_eq!(
            authenticate(Some(&header), None, NOW),
            Err(AuthError::NotConfigured)
        );
        assert_eq!(
            authenticate(Some(&header), Some(""), NOW),
            Err(AuthError::NotConfigured)
        );
        assert_eq!(
            authenticate(Some(&header), Some(&SECRET[..MIN_SECRET_BYTES - 1]), NOW),
            Err(AuthError::NotConfigured)
        );
    }

    /// TWO-331: the api Worker's minter (`workers/api/src/ingest_grant.rs`)
    /// pins the same file, so both sides agree byte for byte.
    #[test]
    fn shared_api_vectors_reproduce_and_verify() {
        let doc: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/ingest-caller-grant/vectors.json"
        ))
        .expect("vectors parse");
        let vectors = doc["vectors"].as_array().expect("vectors array");
        assert!(vectors.len() >= 4);
        for v in vectors {
            let name = v["name"].as_str().unwrap();
            let secret = v["secret"].as_str().unwrap();
            let user_id = v["user_id"].as_str().unwrap();
            let now = v["now_unix"].as_u64().unwrap();
            let token = format!(
                "v1.{}.{}",
                v["payload_hex"].as_str().unwrap(),
                v["mac_hex"].as_str().unwrap()
            );
            assert_eq!(
                issue(
                    user_id,
                    INGEST_WRITE_SCOPE,
                    v["exp_unix"].as_u64().unwrap(),
                    secret.as_bytes()
                ),
                token,
                "{name}"
            );
            assert_eq!(
                authenticate(Some(&format!("Bearer {token}")), Some(secret), now),
                Ok(Caller {
                    user_id: user_id.to_string()
                }),
                "{name}"
            );
        }
    }

    #[test]
    fn body_user_must_match_the_caller() {
        let caller = Caller {
            user_id: "user_01".to_string(),
        };
        assert_eq!(bind_body_user(&caller, None), Ok(()));
        assert_eq!(bind_body_user(&caller, Some("user_01")), Ok(()));
        assert!(bind_body_user(&caller, Some("user_02")).is_err());
        assert!(bind_body_user(&caller, Some("")).is_err());
    }

    #[test]
    fn job_ownership_is_exact() {
        let caller = Caller {
            user_id: "user_01".to_string(),
        };
        assert!(owns_job(&caller, "user_01"));
        assert!(!owns_job(&caller, "user_02"));
        assert!(!owns_job(&caller, "user_0"));
        assert!(!owns_job(&caller, "user_011"));
        assert!(!owns_job(&caller, ""));
    }
}
