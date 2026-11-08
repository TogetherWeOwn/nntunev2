//! ListenBrainz wire contract, with no network implementation or user route.
//!
//! Protocol references: the public ListenBrainz API `core.html`,
//! `recordings.html` and `feedback-json.html` documentation. Tokens travel
//! only in Authorization, never in a URL or JSON body. A future transport
//! must disable redirects, bound response collection before allocation,
//! enforce a total deadline, and atomically claim the shared rate budget
//! before sending. It must never log request headers or upstream bodies.

use serde::Deserialize;
use zeroize::Zeroizing;

use crate::listenbrainz::{Feedback, Token, Username};

pub const VALIDATE_TOKEN_URL: &str = "https://api.listenbrainz.org/1/validate-token";
pub const RECORDING_FEEDBACK_URL: &str =
    "https://api.listenbrainz.org/1/feedback/recording-feedback";
pub const MAX_VALIDATION_BODY_BYTES: usize = 4096;
pub const TOTAL_TIMEOUT_MS: u64 = 5000;
pub const ALLOW_REDIRECTS: bool = false;

/// Owns the short-lived plaintext token. No Serialize or Clone; Debug
/// deliberately omits headers. The token is zeroized when this is dropped.
pub struct ApiRequest {
    method: &'static str,
    url: &'static str,
    token: Token,
    body: Option<String>,
}

impl std::fmt::Debug for ApiRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("authorization", &"<redacted>")
            .finish()
    }
}

impl ApiRequest {
    pub fn validation(token: Token) -> Self {
        Self {
            method: "GET",
            url: VALIDATE_TOKEN_URL,
            token,
            body: None,
        }
    }

    pub fn feedback(token: Token, feedback: &Feedback) -> Result<Self, WireError> {
        Ok(Self {
            method: "POST",
            url: RECORDING_FEEDBACK_URL,
            token,
            body: Some(serde_json::to_string(feedback).map_err(|_| WireError::InvalidResponse)?),
        })
    }

    pub fn method(&self) -> &'static str {
        self.method
    }

    pub fn url(&self) -> &'static str {
        self.url
    }

    pub fn body(&self) -> Option<&str> {
        self.body.as_deref()
    }

    /// Server-side transport only; never put this value in diagnostics.
    pub fn authorization(&self) -> Zeroizing<String> {
        Zeroizing::new(format!("Token {}", self.token.expose()))
    }
}

/// Scalar-only errors; upstream bodies and credentials must not be retained
/// in an error, returned to a browser, or interpolated into a log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    Rejected,
    RateLimited,
    Unavailable,
    InvalidResponse,
}

fn require_success(status: u16) -> Result<(), WireError> {
    match status {
        200 => Ok(()),
        400 | 401 | 403 => Err(WireError::Rejected),
        429 => Err(WireError::RateLimited),
        _ => Err(WireError::Unavailable),
    }
}

#[derive(Deserialize)]
struct Validation {
    valid: bool,
    user_name: Option<String>,
}

/// HTTP 200 alone does not validate a token: ListenBrainz returns 200 for
/// `valid: false` too. Linking additionally requires the returned account
/// to match the explicitly supplied username, never a silently different
/// account. Returns the upstream's canonical spelling. No token is saved
/// until this succeeds and the caller commits the encrypted replacement.
pub fn validated_username(
    status: u16,
    body: &[u8],
    expected: &Username,
) -> Result<Username, WireError> {
    require_success(status)?;
    if body.len() > MAX_VALIDATION_BODY_BYTES {
        return Err(WireError::InvalidResponse);
    }
    let reply: Validation = serde_json::from_slice(body).map_err(|_| WireError::InvalidResponse)?;
    if !reply.valid {
        return Err(WireError::Rejected);
    }
    let name = reply.user_name.ok_or(WireError::InvalidResponse)?;
    if !name.eq_ignore_ascii_case(expected.as_str()) {
        return Err(WireError::Rejected);
    }
    Username::parse(&name).map_err(|_| WireError::InvalidResponse)
}

/// The recording-feedback API documents 200 as acceptance; response bodies
/// are not forwarded or retained. This is not local taste persistence.
pub fn feedback_accepted(status: u16) -> Result<(), WireError> {
    require_success(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::listenbrainz::{feedback_for, Taste};

    const TOKEN: &str = "0b9e2f4a-1c3d-4e5f-8a6b-7c8d9e0f1a2b";
    const MBID: &str = "8f3471b5-7e6a-48da-86a9-c1c07a0f47ae";

    #[test]
    fn requests_pin_origin_and_keep_tokens_out_of_urls_bodies_and_debug() {
        let request = ApiRequest::validation(Token::parse(TOKEN).unwrap());
        assert_eq!(request.method(), "GET");
        assert_eq!(request.url(), VALIDATE_TOKEN_URL);
        assert!(request.body().is_none());
        assert_eq!(request.authorization().as_str(), format!("Token {TOKEN}"));
        assert!(!format!("{request:?}").contains(TOKEN));
        assert!(!request.url().contains(TOKEN));

        for (taste, score) in [(Taste::Love, 1), (Taste::Dislike, -1), (Taste::Clear, 0)] {
            let feedback = feedback_for(taste, MBID).unwrap().unwrap();
            let request = ApiRequest::feedback(Token::parse(TOKEN).unwrap(), &feedback).unwrap();
            assert_eq!(request.method(), "POST");
            assert_eq!(request.url(), RECORDING_FEEDBACK_URL);
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(request.body().unwrap()).unwrap(),
                serde_json::json!({"recording_mbid": MBID, "score": score})
            );
            assert!(!request.body().unwrap().contains(TOKEN));
            assert!(!format!("{request:?}").contains(TOKEN));
        }
    }

    #[test]
    fn validation_requires_valid_and_matching_account_not_just_http_200() {
        let expected = Username::parse("alice").unwrap();
        let name =
            validated_username(200, br#"{"valid":true,"user_name":"Alice"}"#, &expected).unwrap();
        assert_eq!(name.as_str(), "Alice");
        for body in [
            br#"{"valid":false}"#.as_slice(),
            br#"{"valid":true,"user_name":"other"}"#,
        ] {
            assert_eq!(
                validated_username(200, body, &expected),
                Err(WireError::Rejected)
            );
        }
        for body in [
            br#"{"valid":true}"#.as_slice(),
            br#"{"valid":"true","user_name":"alice"}"#,
            br#"{"valid":true,"user_name":null}"#,
            b"not json",
            br#"{"valid":true,"valid":false,"user_name":"alice"}"#,
        ] {
            assert_eq!(
                validated_username(200, body, &expected),
                Err(WireError::InvalidResponse)
            );
        }
    }

    #[test]
    fn upstream_errors_are_scalar_and_validation_body_is_bounded() {
        let expected = Username::parse("alice").unwrap();
        for (status, error) in [
            (301, WireError::Unavailable),
            (401, WireError::Rejected),
            (429, WireError::RateLimited),
            (503, WireError::Unavailable),
        ] {
            assert_eq!(
                validated_username(status, TOKEN.as_bytes(), &expected),
                Err(error)
            );
            assert_eq!(feedback_accepted(status), Err(error));
            assert!(!format!("{error:?}").contains(TOKEN));
        }
        assert_eq!(feedback_accepted(200), Ok(()));
        assert_eq!(
            validated_username(200, &vec![b' '; MAX_VALIDATION_BODY_BYTES + 1], &expected),
            Err(WireError::InvalidResponse)
        );
    }
}
