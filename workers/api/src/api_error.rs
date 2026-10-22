//! Structured error mappings for the API worker: see `docs/api-errors.md`.
//! The registry test checks that this worker's declared codes stay in sync.
//!
//! Wire shape: `{"code": "...", "message": "...", "retryable": bool}` with
//! the registry HTTP status. Error messages on 5xx paths are sealed to fixed
//! public text, and auth/session failures remain generic.

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
        code: "invalid_request",
        status: 400,
        retryable: false,
    },
    CodeSpec {
        code: "unauthorized",
        status: 401,
        retryable: false,
    },
    CodeSpec {
        code: "reauth_required",
        status: 401,
        retryable: false,
    },
    CodeSpec {
        code: "provider_auth_expired",
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
        code: "account_deleting",
        status: 409,
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

fn message_for_server_error(code: &str) -> &'static str {
    match code {
        "service_unavailable" => "service unavailable",
        _ => "unexpected error",
    }
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
    /// fallback. Registered 5xx codes use fixed public text; other
    /// registered codes keep the caller's message.
    pub fn by_code(code: &str, message: impl Into<String>) -> Self {
        let spec = spec(code);
        let message = if spec.code != code {
            "unexpected error".to_string()
        } else if spec.status >= 500 {
            message_for_server_error(spec.code).to_string()
        } else {
            message.into()
        };
        Self {
            code: spec.code,
            message,
            retryable: spec.retryable,
            status: spec.status,
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::by_code("invalid_request", message)
    }
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::by_code("unauthorized", message)
    }
    pub fn provider_auth_expired(message: impl Into<String>) -> Self {
        Self::by_code("provider_auth_expired", message)
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
    pub fn account_deleting(message: impl Into<String>) -> Self {
        Self::by_code("account_deleting", message)
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
            LibraryError::AccountDeleting => Self::account_deleting(e.to_string()),
            LibraryError::Invalid => Self::validation_failed(e.to_string()),
            LibraryError::RandomnessUnavailable => Self::service_unavailable(e.to_string()),
        }
    }
}

/// Credential failures are one generic 401: the type cannot express which
/// check failed, so neither can the wire code (no enumeration oracle).
/// Server-side RP/origin configuration failures map separately to 500.
impl From<AuthError> for ApiError {
    fn from(e: AuthError) -> Self {
        match e {
            AuthError::Invalid => Self::unauthorized(e.to_string()),
            AuthError::Misconfigured => Self::internal_error(e.to_string()),
            AuthError::RandomnessUnavailable => Self::service_unavailable(e.to_string()),
        }
    }
}

impl From<SessionError> for ApiError {
    fn from(e: SessionError) -> Self {
        match e {
            SessionError::Invalid => Self::unauthorized(e.to_string()),
            SessionError::ReauthRequired => Self::by_code(e.code(), e.to_string()),
            SessionError::Misconfigured | SessionError::Storage { .. } => {
                Self::internal_error(e.to_string())
            }
            SessionError::RandomnessUnavailable => Self::service_unavailable(e.to_string()),
        }
    }
}

impl From<VaultError> for ApiError {
    fn from(e: VaultError) -> Self {
        match e {
            VaultError::Invalid => Self::validation_failed("invalid vault configuration"),
            VaultError::CorruptStoredData | VaultError::Crypto => {
                Self::internal_error(e.to_string())
            }
            VaultError::RandomnessUnavailable => Self::service_unavailable(e.to_string()),
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
        // The deletion fence is a stable, non-retryable 409 with its own
        // code: clients must not see it as a retryable 5xx or a 404.
        let deleting = ApiError::from(LibraryError::AccountDeleting);
        assert_eq!(
            (deleting.code, deleting.status, deleting.retryable),
            ("account_deleting", 409, false)
        );
        assert_eq!(
            body(&deleting),
            serde_json::json!({"code":"account_deleting","message":"account is being deleted","retryable":false})
        );
        assert_eq!(ApiError::from(AuthError::Invalid).code, "unauthorized");
        let auth_config = ApiError::from(AuthError::Misconfigured);
        assert_eq!(
            (auth_config.code, auth_config.status),
            ("internal_error", 500)
        );
        assert_eq!(ApiError::from(SessionError::Invalid).code, "unauthorized");
        let reauth_required = ApiError::from(SessionError::ReauthRequired);
        assert_eq!(
            (
                reauth_required.code,
                reauth_required.status,
                reauth_required.retryable
            ),
            ("reauth_required", 401, false)
        );
        let session_config = ApiError::from(SessionError::Misconfigured);
        assert_eq!(
            (session_config.code, session_config.status),
            ("internal_error", 500)
        );
        let invalid_vault = ApiError::from(VaultError::Invalid);
        assert_eq!(
            (invalid_vault.code, invalid_vault.message.as_str()),
            ("validation_failed", "invalid vault configuration")
        );
        let corrupt_vault = ApiError::from(VaultError::CorruptStoredData);
        assert_eq!(
            (corrupt_vault.code, corrupt_vault.status),
            ("internal_error", 500)
        );
        let crypto = ApiError::from(VaultError::Crypto);
        assert_eq!(
            (crypto.code, crypto.status, crypto.message.as_str()),
            ("internal_error", 500, "unexpected error")
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
    fn server_error_messages_are_sealed_to_public_text() {
        let err = ApiError::internal_error("database password and provider response");
        assert_eq!(
            body(&err),
            serde_json::json!({"code":"internal_error","message":"unexpected error","retryable":true})
        );
        let unavailable = ApiError::service_unavailable("binding token");
        assert_eq!(unavailable.message, "service unavailable");
    }

    #[test]
    fn provider_auth_expired_uses_its_registered_reauth_code() {
        let err = ApiError::provider_auth_expired("provider credentials expired");
        assert_eq!(
            (err.code, err.status, err.retryable),
            ("provider_auth_expired", 401, false)
        );
        assert_eq!(err.message, "provider credentials expired");
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
            let owners: Vec<&str> = cells[4].split(',').map(str::trim).collect();
            let mine = REGISTRY.iter().find(|s| s.code == code);
            // Reverse direction: a row naming this worker must be a code
            // declared by its mapping module. This does not assert that an
            // HTTP handler currently emits the mapping.
            assert_eq!(
                owners.contains(&"api"),
                mine.is_some(),
                "owning-worker drift for {code}"
            );
            if let Some(mine) = mine {
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
            "every declared code must appear in docs/api-errors.md"
        );
    }
}
