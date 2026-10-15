//! Acceptance tests: structured error taxonomy for the `stream` worker.
//!
//! TOG-25314 (test-only, TDD RED): these tests FAIL until TOG-25315
//! implements the contract below. The builder builds on this branch and
//! must never edit this file: implement to satisfy it.
//!
//! ## Builder contract (TOG-25315)
//! In `workers/stream/src/errors.rs`, exposed as `pub mod errors` from `lib.rs`.
//! NOTE: `nntune-stream` is dependency-free (no serde/serde_json): keep it
//! that way — hand-render the JSON or return (&'static str parts). The
//! minimal surface this file requires:
//! ```ignore
//! pub struct ErrorEnvelope {
//!     pub code: &'static str,      // SCREAMING_SNAKE registry code
//!     pub message: &'static str,  // user-facing, non-empty, no internals/secrets
//!     pub retryable: bool,         // client may retry (respects Retry-After where sent)
//!     pub status: u16,             // canonical HTTP status for `code`
//! }
//! impl ErrorEnvelope {
//!     /// Body bytes: exactly {"code","message","retryable"} (never an `error` key).
//!     pub fn to_json_string(&self) -> String; // String is core-alloc: fine without serde
//! }
//! /// Look up a code; unknown codes fall back to INTERNAL (500, non-retryable).
//! pub fn envelope_for(code: &str) -> ErrorEnvelope;
//! /// Every registered code (superset of the pinned table below).
//! pub const ALL_CODES: &[&str];
//! impl From<grant::GrantError> for ErrorEnvelope;
//! // Malformed | BadSignature | Expired -> UNAUTHENTICATED (401, non-retryable)
//! impl From<play::PlayError> for ErrorEnvelope;
//! // Unauthorized(_)              -> UNAUTHENTICATED (401, non-retryable)
//! // WrongTrack | InvalidSegment(_) | RangeUnsatisfiable -> 400/416 per table, non-retryable
//! //   (RangeUnsatisfiable -> RANGE_UNSATISFIABLE 416)
//! // ArticleMissing(_)            -> NOT_FOUND (404, non-retryable; pool stays warm)
//! // PoolExhausted                -> RATE_LIMITED (429, retryable)
//! // PoolQuarantined { .. } | Upstream(_) -> UPSTREAM_UNAVAILABLE (503, retryable)
//! // Decode(_)                    -> INTERNAL (500, non-retryable, generic message)
//! ```
//! Registry doc `docs/api-errors.md`: a `| Code | Status | Retryable | Owner | ... |`
//! table plus a note that unknown codes fall back to INTERNAL.
//!
//! ## Pinned code table (shared across all four worker test files)
//! | code                   | status | retryable | owners               |
//! |------------------------|--------|-----------|----------------------|
//! | UNAUTHENTICATED        | 401    | false     | api, stream          |
//! | VALIDATION_ERROR       | 400    | false     | api, ingest, mb, stream |
//! | NOT_FOUND              | 404    | false     | ingest, mb, stream   |
//! | PAYLOAD_TOO_LARGE      | 413    | false     | ingest               |
//! | RANGE_UNSATISFIABLE    | 416    | false     | stream               |
//! | RATE_LIMITED           | 429    | true      | ingest, stream       |
//! | BILLING_QUOTA_EXCEEDED | 402    | false     | api                  |
//! | UPSTREAM_UNAVAILABLE   | 503    | true      | ingest, mb, stream   |
//! | INTERNAL               | 500    | false     | all (unknown-code fallback) |

use nntune_stream::errors::{envelope_for, ErrorEnvelope, ALL_CODES};
use nntune_stream::grant::GrantError;
use nntune_stream::play::PlayError;
use nntune_stream::yenc::YencError;

// --- shared pinned table -----------------------------------------------------

/// (code, status, retryable): the frozen cross-worker contract.
const PINNED: &[(&str, u16, bool)] = &[
    ("UNAUTHENTICATED", 401, false),
    ("VALIDATION_ERROR", 400, false),
    ("NOT_FOUND", 404, false),
    ("PAYLOAD_TOO_LARGE", 413, false),
    ("RANGE_UNSATISFIABLE", 416, false),
    ("RATE_LIMITED", 429, true),
    ("BILLING_QUOTA_EXCEEDED", 402, false),
    ("UPSTREAM_UNAVAILABLE", 503, true),
    ("INTERNAL", 500, false),
];

/// Expected doc owners per code (each row must name >= 1, or "all").
fn expected_owners(code: &str) -> &'static [&'static str] {
    match code {
        "UNAUTHENTICATED" => &["api", "stream"],
        "VALIDATION_ERROR" => &["api", "ingest", "mb", "stream"],
        "NOT_FOUND" => &["ingest", "mb", "stream"],
        "PAYLOAD_TOO_LARGE" => &["ingest"],
        "RANGE_UNSATISFIABLE" => &["stream"],
        "RATE_LIMITED" => &["ingest", "stream"],
        "BILLING_QUOTA_EXCEEDED" => &["api"],
        "UPSTREAM_UNAVAILABLE" => &["ingest", "mb", "stream"],
        "INTERNAL" => &["all"],
        _ => &[],
    }
}

/// Substrings that must never appear in a user-facing message (case-insensitive).
const LEAK_MARKERS: &[&str] = &[
    "panic",
    "unwrap",
    "expect(",
    "sql",
    "postgres",
    "tokio",
    "hyperdrive",
    "backtrace",
    "secret",
    "password",
    "api_key",
    "apikey",
    "token_hash",
];

fn assert_user_facing(message: &str, code: &str) {
    assert!(
        !message.trim().is_empty(),
        "{code}: message must be non-empty"
    );
    let lower = message.to_lowercase();
    for marker in LEAK_MARKERS {
        assert!(
            !lower.contains(marker),
            "{code}: message leaks internals ({marker}): {message:?}"
        );
    }
}

/// Minimal JSON-shape assertion without serde (this crate is dependency-free):
/// the body must carry "code", "message" and "retryable" keys and must not
/// carry the legacy "error" key. The api/ingest/mb twins assert full
/// value-equality via serde_json; here we assert presence + code echo.
fn assert_envelope_shape(env: &ErrorEnvelope, body: &str) {
    let body = body.trim();
    assert!(
        body.starts_with('{') && body.ends_with('}'),
        "envelope body must be a JSON object: {body}"
    );
    for key in [r#""code""#, r#""message""#, r#""retryable""#] {
        assert!(body.contains(key), "body must contain {key}: {body}");
    }
    assert!(
        !body.contains(r#""error""#),
        "legacy {{\"error\": ...}} shape is banned: {body}"
    );
    assert!(
        body.contains(env.code),
        "body must echo the code {}: {body}",
        env.code
    );
}

// --- per-route mapping (stream worker: grants + play path) --------------------

#[test]
fn grant_rejections_map_to_unauthenticated_401() {
    for err in [
        GrantError::Malformed,
        GrantError::BadSignature,
        GrantError::Expired,
    ] {
        let env = ErrorEnvelope::from(err.clone());
        assert_eq!(env.code, "UNAUTHENTICATED", "{err:?}");
        assert_eq!(env.status, 401, "{err:?}");
        assert!(!env.retryable, "{err:?}: re-authenticate, do not retry");
        assert_user_facing(env.message, env.code);
        assert_envelope_shape(&env, &env.to_json_string());
    }
}

#[test]
fn play_auth_and_client_errors_map_to_4xx_without_retry() {
    let cases: Vec<(PlayError, &str, u16)> = vec![
        (
            PlayError::Unauthorized(GrantError::Expired),
            "UNAUTHENTICATED",
            401,
        ),
        (PlayError::WrongTrack, "VALIDATION_ERROR", 400),
        (
            PlayError::InvalidSegment("bad id".to_string()),
            "VALIDATION_ERROR",
            400,
        ),
        (PlayError::RangeUnsatisfiable, "RANGE_UNSATISFIABLE", 416),
    ];
    for (err, code, status) in cases {
        let env = ErrorEnvelope::from(err);
        assert_eq!(env.code, code);
        assert_eq!(env.status, status, "{code}");
        assert!(!env.retryable, "{code}: client-side, retry is pointless");
        assert_user_facing(env.message, code);
        assert_envelope_shape(&env, &env.to_json_string());
    }
}

#[test]
fn gone_article_maps_to_not_found_without_indicting_the_pool() {
    // 430 says nothing about the provider: 404, non-retryable for this
    // track (other tracks on the warm pool are unaffected).
    let env = ErrorEnvelope::from(PlayError::ArticleMissing("<gone@x>".to_string()));
    assert_eq!(env.code, "NOT_FOUND");
    assert_eq!(env.status, 404);
    assert!(!env.retryable);
    assert_user_facing(env.message, env.code);
    assert!(
        !env.message.contains("<gone@x>"),
        "message must not echo the segment id: {:?}",
        env.message
    );
    assert_envelope_shape(&env, &env.to_json_string());
}

#[test]
fn pool_pressure_maps_to_retryable_429_and_503() {
    let exhausted = ErrorEnvelope::from(PlayError::PoolExhausted);
    assert_eq!(exhausted.code, "RATE_LIMITED");
    assert_eq!(exhausted.status, 429);
    assert!(exhausted.retryable);
    assert_user_facing(exhausted.message, exhausted.code);
    assert_envelope_shape(&exhausted, &exhausted.to_json_string());

    for err in [
        PlayError::PoolQuarantined {
            retry_after_ms: 5_000,
        },
        PlayError::Upstream("dial failed".to_string()),
    ] {
        let env = ErrorEnvelope::from(err);
        assert_eq!(env.code, "UPSTREAM_UNAVAILABLE");
        assert_eq!(env.status, 503);
        assert!(env.retryable);
        assert_user_facing(env.message, env.code);
        assert_envelope_shape(&env, &env.to_json_string());
    }
}

#[test]
fn decode_failure_maps_to_internal_without_detail() {
    let env = ErrorEnvelope::from(PlayError::Decode(YencError::CrcMismatch {
        expected: 1,
        actual: 2,
    }));
    assert_eq!(env.code, "INTERNAL");
    assert_eq!(env.status, 500);
    assert!(!env.retryable);
    assert_user_facing(env.message, env.code);
    assert_envelope_shape(&env, &env.to_json_string());
}

// --- shared contract: lookup, shape, fallback ---------------------------------

#[test]
fn every_pinned_code_resolves_to_its_status_and_flag() {
    for (code, status, retryable) in PINNED {
        let env = envelope_for(code);
        assert_eq!(env.code, *code, "lookup must echo the code");
        assert_eq!(env.status, *status, "{code}: status");
        assert_eq!(env.retryable, *retryable, "{code}: retryable");
        assert_user_facing(env.message, code);
        assert_envelope_shape(&env, &env.to_json_string());
    }
}

#[test]
fn unknown_code_falls_back_to_internal_generic() {
    let env = envelope_for("NO_SUCH_CODE_XYZ");
    assert_eq!(env.code, "INTERNAL");
    assert_eq!(env.status, 500);
    assert!(!env.retryable);
    assert_user_facing(env.message, env.code);
    assert!(
        !env.message.contains("NO_SUCH_CODE_XYZ"),
        "fallback message must stay generic: {:?}",
        env.message
    );
    assert_envelope_shape(&env, &env.to_json_string());
}

#[test]
fn all_codes_covers_the_pinned_table() {
    for (code, _, _) in PINNED {
        assert!(
            ALL_CODES.contains(code),
            "ALL_CODES must contain pinned {code}"
        );
    }
}

// --- registry doc coverage ----------------------------------------------------

struct RegRow {
    code: String,
    status: Option<u16>,
    retryable: Option<bool>,
    owner: String,
}

fn find_registry() -> String {
    let mut dir = std::env::current_dir().expect("cwd");
    for _ in 0..6 {
        let cand = dir.join("docs/api-errors.md");
        if cand.is_file() {
            return std::fs::read_to_string(&cand).expect("read registry");
        }
        if !dir.pop() {
            break;
        }
    }
    panic!(
        "docs/api-errors.md not found above {:?}",
        std::env::current_dir()
    );
}

fn parse_registry(text: &str) -> Vec<RegRow> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = line
            .split('|')
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .collect();
        if cells.len() < 4 {
            continue;
        }
        if cells[0].eq_ignore_ascii_case("code") {
            continue; // header
        }
        if cells[0].chars().all(|c| c == '-' || c == ':' || c == ' ') {
            continue; // separator
        }
        let code = cells[0].trim_matches('`').trim().to_string();
        if code.is_empty()
            || !code
                .chars()
                .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit())
        {
            continue;
        }
        let status = cells[1].trim_matches('`').trim().parse::<u16>().ok();
        let retryable = match cells[2].trim_matches('`').trim().to_lowercase().as_str() {
            "yes" | "true" | "1" => Some(true),
            "no" | "false" | "0" => Some(false),
            _ => None,
        };
        rows.push(RegRow {
            code,
            status,
            retryable,
            owner: cells[3].to_lowercase(),
        });
    }
    rows
}

#[test]
fn registry_doc_lists_every_pinned_code_with_status_flag_and_owner() {
    let rows = parse_registry(&find_registry());
    assert!(!rows.is_empty(), "registry table must parse to >= 1 row");
    for (code, status, retryable) in PINNED {
        let row = rows
            .iter()
            .find(|r| r.code == *code)
            .unwrap_or_else(|| panic!("registry must list {code}"));
        assert_eq!(row.status, Some(*status), "{code}: registry status");
        assert_eq!(
            row.retryable,
            Some(*retryable),
            "{code}: registry retryable"
        );
        let owners = expected_owners(code);
        assert!(
            owners.iter().any(|o| row.owner.contains(o)) || row.owner.contains("all"),
            "{code}: registry owner must name one of {owners:?} (got {:?})",
            row.owner
        );
    }
}

#[test]
fn registry_doc_covers_every_registered_code_and_documents_fallback() {
    let text = find_registry();
    let rows = parse_registry(&text);
    for code in ALL_CODES {
        assert!(
            rows.iter().any(|r| r.code == *code),
            "every ALL_CODES entry must appear in the doc: {code}"
        );
    }
    let lower = text.to_lowercase();
    assert!(
        lower.contains("fallback") && text.contains("INTERNAL"),
        "doc must document the unknown-code -> INTERNAL fallback"
    );
}
