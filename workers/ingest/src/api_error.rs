//! Structured error mappings for the ingest worker: see `docs/api-errors.md`.
//! The registry test checks that this worker's declared codes stay in sync.
//!
//! Wire shape: `{"code": "...", "message": "...", "retryable": bool}` with
//! the registry HTTP status. Validation messages are user-facing by
//! construction; 5xx messages never carry binding or upstream detail.

use serde::Serialize;

use crate::nzb_intake::IntakeRejection;

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
        code: "plan_limit_exceeded",
        status: 402,
        retryable: false,
    },
    CodeSpec {
        code: "not_found",
        status: 404,
        retryable: false,
    },
    CodeSpec {
        code: "conflict",
        status: 409,
        retryable: false,
    },
    CodeSpec {
        code: "invalid_request",
        status: 400,
        retryable: false,
    },
    CodeSpec {
        code: "payload_too_large",
        status: 413,
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
        code: "upstream_unavailable",
        status: 502,
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
        "upstream_unavailable" => "upstream unavailable",
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
    pub fn plan_limit_exceeded(message: impl Into<String>) -> Self {
        Self::by_code("plan_limit_exceeded", message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::by_code("not_found", message)
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::by_code("conflict", message)
    }
    pub fn payload_too_large(message: impl Into<String>) -> Self {
        Self::by_code("payload_too_large", message)
    }
    pub fn rate_limited(message: impl Into<String>) -> Self {
        Self::by_code("rate_limited", message)
    }
    pub fn internal_error(message: impl Into<String>) -> Self {
        Self::by_code("internal_error", message)
    }
    pub fn upstream_unavailable(message: impl Into<String>) -> Self {
        Self::by_code("upstream_unavailable", message)
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

impl From<IntakeRejection> for ApiError {
    fn from(e: IntakeRejection) -> Self {
        match e {
            IntakeRejection::Empty | IntakeRejection::NotNzb | IntakeRejection::Malformed => {
                Self::invalid_request(e.message())
            }
            IntakeRejection::TooLarge { .. }
            | IntakeRejection::TooManyFiles { .. }
            | IntakeRejection::TooManySegments { .. }
            | IntakeRejection::DeclaredBytesOverCap { .. } => Self::payload_too_large(e.message()),
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
        let err = ApiError::invalid_request("query must be 1..=200 chars");
        assert_eq!(err.status, 400);
        assert_eq!(
            body(&err),
            serde_json::json!({"code":"invalid_request","message":"query must be 1..=200 chars","retryable":false})
        );
    }

    #[test]
    fn intake_rejections_map_to_registered_codes() {
        assert_eq!(
            (
                ApiError::from(IntakeRejection::Empty).code,
                ApiError::from(IntakeRejection::Empty).status
            ),
            ("invalid_request", 400)
        );
        let too_large = IntakeRejection::TooLarge {
            size_bytes: 3,
            max_bytes: 2,
        };
        assert_eq!(
            (
                ApiError::from(too_large).code,
                ApiError::from(too_large).status
            ),
            ("payload_too_large", 413)
        );
        assert_eq!(
            ApiError::from(IntakeRejection::NotNzb).code,
            "invalid_request"
        );
        assert_eq!(
            ApiError::from(IntakeRejection::TooManyFiles { count: 6, max: 5 }).code,
            "payload_too_large"
        );
        assert_eq!(
            ApiError::from(IntakeRejection::TooManySegments { count: 61, max: 60 }).code,
            "payload_too_large"
        );
        assert_eq!(
            ApiError::from(IntakeRejection::DeclaredBytesOverCap {
                declared: 11,
                max: 10,
            })
            .code,
            "payload_too_large"
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
            let code = cells[1].trim_matches('`');
            let status: u16 = cells[2].parse().expect("registry status parses");
            let retryable: bool = cells[3].parse().expect("registry retryable parses");
            let owners: Vec<&str> = cells[4].split(',').map(str::trim).collect();
            let mine = REGISTRY.iter().find(|s| s.code == code);
            // Reverse direction: a row naming this worker must be a code
            // declared by its mapping module. This does not assert that an
            // HTTP handler currently emits the mapping.
            assert_eq!(
                owners.contains(&"ingest"),
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
