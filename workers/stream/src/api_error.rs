//! Structured error envelope shared by every worker: see
//! `docs/api-errors.md` (the registry is the source of truth; the doc-lint
//! test below fails if this module drifts from it).
//!
//! Wire shape: `{"code": "...", "message": "...", "retryable": bool}` with
//! the registry HTTP status. This crate stays dependency-free, so the
//! envelope JSON is escaped by hand below. Messages are static per code:
//! provider replies, message-ids, and decode detail never reach `message`.

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
        code: "invalid_request",
        status: 400,
        retryable: false,
    },
    CodeSpec {
        code: "range_not_satisfiable",
        status: 416,
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
        code: "corrupt_content",
        status: 502,
        retryable: false,
    },
    CodeSpec {
        code: "upstream_unavailable",
        status: 502,
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
/// status, [`ApiError::body_json`] is exactly `{code, message, retryable}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub code: &'static str,
    pub message: &'static str,
    pub retryable: bool,
    pub status: u16,
}

impl ApiError {
    /// Build from a registry code; unknown codes become the generic
    /// fallback (code, status, and message).
    pub fn by_code(code: &str, message: &'static str) -> Self {
        let spec = spec(code);
        let message = if spec.code == code {
            message
        } else {
            "unexpected error"
        };
        Self {
            code: spec.code,
            message,
            retryable: spec.retryable,
            status: spec.status,
        }
    }

    pub fn unauthorized() -> Self {
        Self::by_code("unauthorized", "authentication required")
    }
    pub fn plan_limit_exceeded() -> Self {
        Self::by_code("plan_limit_exceeded", "plan limit reached")
    }
    pub fn forbidden() -> Self {
        Self::by_code("forbidden", "not the owner")
    }
    pub fn not_found() -> Self {
        Self::by_code("not_found", "not found")
    }
    pub fn invalid_request() -> Self {
        Self::by_code("invalid_request", "invalid request")
    }
    pub fn range_not_satisfiable() -> Self {
        Self::by_code("range_not_satisfiable", "range not satisfiable")
    }
    pub fn rate_limited() -> Self {
        Self::by_code("rate_limited", "too many requests")
    }
    pub fn internal_error() -> Self {
        Self::by_code("internal_error", "unexpected error")
    }
    pub fn corrupt_content() -> Self {
        Self::by_code("corrupt_content", "content could not be decoded")
    }
    pub fn upstream_unavailable() -> Self {
        Self::by_code("upstream_unavailable", "provider unavailable")
    }

    /// Serialized envelope body.
    pub fn body_json(&self) -> String {
        format!(
            "{{\"code\":\"{}\",\"message\":\"{}\",\"retryable\":{}}}",
            escape_json(self.code),
            escape_json(self.message),
            self.retryable
        )
    }
}

/// Escape a string for a JSON string literal (quotes, backslash, controls).
fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_is_exactly_code_message_retryable() {
        let err = ApiError::unauthorized();
        assert_eq!(err.status, 401);
        assert_eq!(
            err.body_json(),
            r#"{"code":"unauthorized","message":"authentication required","retryable":false}"#
        );
    }

    #[test]
    fn billing_limit_shapes_are_pinned_before_enforcement_lands() {
        let plan = ApiError::plan_limit_exceeded();
        assert_eq!((plan.status, plan.retryable), (402, false));
        assert_eq!(
            plan.body_json(),
            r#"{"code":"plan_limit_exceeded","message":"plan limit reached","retryable":false}"#
        );
        let rate = ApiError::rate_limited();
        assert_eq!((rate.status, rate.retryable), (429, true));
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
    fn json_escaping_covers_quotes_backslashes_and_controls() {
        assert_eq!(escape_json("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
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
