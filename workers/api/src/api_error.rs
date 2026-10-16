//! Structured error envelope shared by every worker: see
//! `docs/api-errors.md` (the registry is the source of truth; the doc-lint
//! test below fails if this module drifts from it).
//!
//! Wire shape: `{"code": "...", "message": "...", "retryable": bool}` with
//! the registry HTTP status. Messages on auth/session/vault paths stay
//! generic by construction: those error types only Display generic text,
//! and this module never formats internal detail into `message`.

use serde::Serialize;

use crate::auth::AuthError;
use crate::library::LibraryError;
use crate::session::SessionError;
use crate::vault::VaultError;

/// One registry row: the HTTP status and retry policy for a `code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeSpec {
    pub code: &'static str,
    pub status: u16,
    pub retryable: bool,
}

/// Codes this worker may emit. Must match `docs/api-errors.md`.
pub const REGISTRY: &[CodeSpec] = &[
    CodeSpec {
        code: "unauthorized",
        status: 401,
        retryable: false,
    },
    CodeSpec {
        code: "plan_limit_exceeded",
        status: 402,
        retryable: false,
    },
    CodeSpec {
        code: "forbidden",
        status: 403,
        retryable: false,
    },
    CodeSpec {
        code: "not_found",
        status: 404,
        retryable: false,
    },
    CodeSpec {
        code: "validation_failed",
        status: 422,
        retryable: false,
    },
    CodeSpec {
        code: "rate_limited",
        status: 429,
        retryable: true,
    },
    CodeSpec {
        code: "internal_error",
        status: 500,
        retryable: true,
    },
    CodeSpec {
        code: "service_unavailable",
        status: 503,
        retryable: true,
    },
];

/// The documented generic every unknown code falls back to.
pub const FALLBACK: CodeSpec = CodeSpec {
    code: "internal_error",
    status: 500,
    retryable: true,
};

/// Look up a code; unregistered codes return [`FALLBACK`].
pub fn spec(code: &str) -> CodeSpec {
    REGISTRY
        .iter()
        .copied()
        .find(|s| s.code == code)
        .unwrap_or(FALLBACK)
}

/// A user-facing error ready for the HTTP layer: `status` is the response
/// status, the serialized body is exactly `{code, message, retryable}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiError {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
    #[serde(skip)]
    pub status: u16,
}

impl ApiError {
    /// Build from a registry code; unknown codes become the generic
    /// fallback (code and status), keeping the caller's message only when
    /// the code was registered.
    pub fn by_code(code: &str, message: impl Into<String>) -> Self {
        let spec = spec(code);
        let message = if spec.code == code {
            message.into()
        } else {
            "unexpected error".to_string()
        };
        Self {
            code: spec.code,
            message,
            retryable: spec.retryable,
            status: spec.status,
        }
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::by_code("unauthorized", message)
    }
    pub fn plan_limit_exceeded(message: impl Into<String>) -> Self {
        Self::by_code("plan_limit_exceeded", message)
    }
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::by_code("forbidden", message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::by_code("not_found", message)
    }
    pub fn validation_failed(message: impl Into<String>) -> Self {
        Self::by_code("validation_failed", message)
    }
    pub fn rate_limited(message: impl Into<String>) -> Self {
        Self::by_code("rate_limited", message)
    }
    pub fn internal_error(message: impl Into<String>) -> Self {
        Self::by_code("internal_error", message)
    }
    pub fn service_unavailable(message: impl Into<String>) -> Self {
        Self::by_code("service_unavailable", message)
    }

    /// Serialized envelope body. Infallible: the shape is three scalars.
    pub fn body_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            r#"{"code":"internal_error","message":"unexpected error","retryable":true}"#.to_string()
        })
    }
}

impl From<LibraryError> for ApiError {
    fn from(e: LibraryError) -> Self {
        match e {
            LibraryError::Unauthorized => Self::unauthorized(e.to_string()),
            LibraryError::Forbidden => Self::forbidden(e.to_string()),
            LibraryError::NotFound => Self::not_found(e.to_string()),
            LibraryError::Invalid => Self::validation_failed(e.to_string()),
        }
    }
}

/// Auth failures are one generic 401: the type cannot express which check
/// failed, so neither can the wire code (no enumeration oracle).
impl From<AuthError> for ApiError {
    fn from(e: AuthError) -> Self {
        Self::unauthorized(e.to_string())
    }
}

impl From<SessionError> for ApiError {
    fn from(e: SessionError) -> Self {
        Self::unauthorized(e.to_string())
    }
}

impl From<VaultError> for ApiError {
    fn from(e: VaultError) -> Self {
        match e {
            VaultError::Invalid => Self::validation_failed(e.to_string()),
            VaultError::Crypto => Self::internal_error(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(err: &ApiError) -> serde_json::Value {
        serde_json::from_str(&err.body_json()).unwrap()
    }

    #[test]
    fn envelope_is_exactly_code_message_retryable() {
        let err = ApiError::from(LibraryError::Unauthorized);
        assert_eq!(err.status, 401);
        assert_eq!(
            body(&err),
            serde_json::json!({"code":"unauthorized","message":"authentication required","retryable":false})
        );
    }

    #[test]
    fn library_auth_session_and_vault_errors_map_to_registered_codes() {
        assert_eq!(
            (
                ApiError::from(LibraryError::Forbidden).code,
                ApiError::from(LibraryError::Forbidden).status
            ),
            ("forbidden", 403)
        );
        assert_eq!(
            (
                ApiError::from(LibraryError::NotFound).code,
                ApiError::from(LibraryError::NotFound).status
            ),
            ("not_found", 404)
        );
        assert_eq!(
            (
                ApiError::from(LibraryError::Invalid).code,
                ApiError::from(LibraryError::Invalid).status
            ),
            ("validation_failed", 422)
        );
        assert_eq!(ApiError::from(AuthError::Invalid).code, "unauthorized");
        assert_eq!(ApiError::from(SessionError::Invalid).code, "unauthorized");
        assert_eq!(
            ApiError::from(VaultError::Invalid).code,
            "validation_failed"
        );
        let crypto = ApiError::from(VaultError::Crypto);
        assert_eq!(
            (crypto.code, crypto.status, crypto.message.as_str()),
            ("internal_error", 500, "vault unavailable")
        );
    }

    #[test]
    fn billing_limit_shapes_are_pinned_before_enforcement_lands() {
        let plan = ApiError::plan_limit_exceeded("plan limit reached");
        assert_eq!((plan.status, plan.retryable), (402, false));
        assert_eq!(body(&plan)["code"], "plan_limit_exceeded");
        let rate = ApiError::rate_limited("too many requests");
        assert_eq!((rate.status, rate.retryable), (429, true));
        assert_eq!(body(&rate)["code"], "rate_limited");
    }

    #[test]
    fn unknown_code_falls_back_to_the_documented_generic() {
        let err = ApiError::by_code("no_such_code", "leaky internal detail");
        assert_eq!(
            (err.code, err.status, err.retryable),
            ("internal_error", 500, true)
        );
        assert_eq!(err.message, "unexpected error");
        assert_eq!(spec("no_such_code"), FALLBACK);
    }

    #[test]
    fn registry_matches_docs_api_errors() {
        let doc = include_str!("../../../docs/api-errors.md");
        let mut parsed = 0;
        for line in doc.lines().filter(|l| l.starts_with("| `")) {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            // cells: ["", "`code`", status, retryable, workers, meaning, ""]
            let code = cells[1].trim_matches('`');
            let status: u16 = cells[2].parse().expect("registry status parses");
            let retryable: bool = cells[3].parse().expect("registry retryable parses");
            if let Some(mine) = REGISTRY.iter().find(|s| s.code == code) {
                assert_eq!(
                    (mine.status, mine.retryable),
                    (status, retryable),
                    "doc drift for {code}"
                );
                parsed += 1;
            }
        }
        assert_eq!(
            parsed,
            REGISTRY.len(),
            "every emitted code must appear in docs/api-errors.md"
        );
    }
}
