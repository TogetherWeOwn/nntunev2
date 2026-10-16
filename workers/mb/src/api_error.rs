//! Structured error mappings for the MB worker: see `docs/api-errors.md`.
//! The registry test checks that this worker's declared codes stay in sync.
//!
//! Wire shape: `{"code": "...", "message": "...", "retryable": bool}` with
//! the registry HTTP status. Repository failures stay a generic
//! `internal_error`: no query or driver detail reaches `message`.

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
        code: "not_found",
        status: 404,
        retryable: false,
    },
    CodeSpec {
        code: "internal_error",
        status: 500,
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

fn message_for_server_error(_code: &str) -> &'static str {
    "unexpected error"
}

/// A user-facing error ready for the HTTP layer: `status` is the response
/// status, the serialized body is exactly `{code, message, retryable}`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
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
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::by_code("not_found", message)
    }
    pub fn internal_error() -> Self {
        Self::by_code("internal_error", "unexpected error")
    }

    /// Serialized envelope body. Infallible: the shape is three scalars.
    pub fn body_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            r#"{"code":"internal_error","message":"unexpected error","retryable":true}"#.to_string()
        })
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
        let err = ApiError::invalid_request("invalid mbid (must be UUID)");
        assert_eq!(err.status, 400);
        assert_eq!(
            body(&err),
            serde_json::json!({"code":"invalid_request","message":"invalid mbid (must be UUID)","retryable":false})
        );
        let internal = ApiError::internal_error();
        assert_eq!(
            body(&internal),
            serde_json::json!({"code":"internal_error","message":"unexpected error","retryable":true})
        );
        let leaky = ApiError::by_code("internal_error", "driver failure: private detail");
        assert_eq!(leaky.message, "unexpected error");
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
                owners.contains(&"mb"),
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
