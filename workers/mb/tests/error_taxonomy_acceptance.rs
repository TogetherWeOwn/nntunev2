//! Acceptance tests: structured error taxonomy for the `mb` worker.
//!
//! TOG-25314 (test-only, TDD RED): these tests FAIL until TOG-25315
//! implements the contract below. The builder builds on this branch and
//! must never edit this file: implement to satisfy it.
//!
//! ## Builder contract (TOG-25315)
//! 1. New `workers/mb/src/errors.rs`, exposed as `pub mod errors`:
//! ```ignore
//! pub struct ErrorEnvelope {
//!     pub code: &'static str,      // SCREAMING_SNAKE registry code
//!     pub message: &'static str,  // user-facing, non-empty, no internals/secrets
//!     pub retryable: bool,         // client may retry (respects Retry-After where sent)
//!     pub status: u16,             // canonical HTTP status for `code`
//! }
//! impl ErrorEnvelope {
//!     /// Body bytes: exactly {"code","message","retryable"} (never an `error` key).
//!     pub fn to_json_string(&self) -> String;
//! }
//! /// Look up a code; unknown codes fall back to INTERNAL (500, non-retryable).
//! pub fn envelope_for(code: &str) -> ErrorEnvelope;
//! /// Every registered code (superset of the pinned table below).
//! pub const ALL_CODES: &[&str];
//! ```
//! 2. Expose the existing testable core (the module docs already call
//!    `handle_lookup` / `handle_search` "the testable core") so error
//!    routes are reachable from integration tests: `pub enum Kind`
//!    (Artist/Release/Recording), `pub trait MbRepo`, `pub trait
//!    SuccessCache`, `pub struct ApiResponse { pub status, pub body, ... }`,
//!    `pub struct RepoError`, `pub enum Entity`, `pub struct SearchHit`,
//!    `pub async fn handle_lookup`, `pub async fn handle_search`.
//!    Keep their current parameter shapes.
//! 3. Route errors through the envelope with this mapping:
//!    - invalid mbid / bad query / bad type -> VALIDATION_ERROR (400, non-retryable)
//!    - missing row                         -> NOT_FOUND (404, non-retryable)
//!    - repo failure (Hyperdrive/Postgres downstream outage) -> UPSTREAM_UNAVAILABLE
//!      (503, retryable; served `no-store`, never cached)
//! 4. Add `rlib` to `[lib] crate-type` (as api/ingest already have) so
//!    `tests/` targets can link the crate.
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

use nntune_mb::errors::{envelope_for, ErrorEnvelope, ALL_CODES};
use nntune_mb::{handle_lookup, handle_search, ApiResponse, Kind, MbRepo, SuccessCache};

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

fn assert_envelope_body(code: &str, status: u16, retryable: bool, resp: &ApiResponse) {
    assert_eq!(resp.status, status, "{code}: HTTP status");
    let v: serde_json::Value = serde_json::from_str(&resp.body).expect("error body must be JSON");
    let obj = v.as_object().expect("error body must be an object");
    assert_eq!(
        obj.get("code").and_then(|c| c.as_str()),
        Some(code),
        "body code must be {code}"
    );
    let message = obj
        .get("message")
        .and_then(|m| m.as_str())
        .expect("body must carry a message");
    assert_user_facing(message, code);
    assert_eq!(
        obj.get("retryable").and_then(|r| r.as_bool()),
        Some(retryable),
        "body retryable must be {retryable}"
    );
    assert!(
        !obj.contains_key("error"),
        "legacy {{\"error\": ...}} shape is banned: {}",
        resp.body
    );
}

// --- minimal test doubles (stateless: error paths never touch the cache) -----

struct RepoDown;
struct RepoEmpty;

impl MbRepo for RepoDown {
    async fn lookup(
        &self,
        _kind: Kind,
        _mbid: &str,
    ) -> Result<Option<nntune_mb::Entity>, nntune_mb::RepoError> {
        Err(nntune_mb::RepoError)
    }

    async fn search(
        &self,
        _kind: Kind,
        _q: &str,
    ) -> Result<Vec<nntune_mb::SearchHit>, nntune_mb::RepoError> {
        Err(nntune_mb::RepoError)
    }
}

impl MbRepo for RepoEmpty {
    async fn lookup(
        &self,
        _kind: Kind,
        _mbid: &str,
    ) -> Result<Option<nntune_mb::Entity>, nntune_mb::RepoError> {
        Ok(None)
    }

    async fn search(
        &self,
        _kind: Kind,
        _q: &str,
    ) -> Result<Vec<nntune_mb::SearchHit>, nntune_mb::RepoError> {
        Ok(vec![])
    }
}

struct NoCache;

impl SuccessCache for NoCache {
    async fn get(&self, _key: &str) -> Option<String> {
        None
    }

    async fn put(&self, _key: &str, _body: &str, _ttl_s: u32) {}
}

/// Minimal executor: the doubles above complete on first poll.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut future = Box::pin(future);
    for _ in 0..100 {
        if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
    }
    panic!("test future did not complete");
}

const MBID: &str = "123e4567-e89b-12d3-a456-426614174000";

// --- per-route mapping (mb worker: lookup + search cores) --------------------

#[test]
fn invalid_mbid_is_a_validation_error_envelope() {
    let resp = block_on(handle_lookup(&RepoDown, &NoCache, Kind::Artist, "nope"));
    assert_envelope_body("VALIDATION_ERROR", 400, false, &resp);
}

#[test]
fn missing_row_is_a_not_found_envelope() {
    let resp = block_on(handle_lookup(&RepoEmpty, &NoCache, Kind::Recording, MBID));
    assert_envelope_body("NOT_FOUND", 404, false, &resp);
}

#[test]
fn repo_failure_is_an_upstream_unavailable_envelope() {
    // A Hyperdrive/Postgres outage is an upstream failure (retryable),
    // never an opaque 500, and never cached as a success.
    let resp = block_on(handle_lookup(&RepoDown, &NoCache, Kind::Artist, MBID));
    assert_envelope_body("UPSTREAM_UNAVAILABLE", 503, true, &resp);

    let resp = block_on(handle_search(
        &RepoDown,
        &NoCache,
        Some("artist".to_string()),
        "anything",
    ));
    assert_envelope_body("UPSTREAM_UNAVAILABLE", 503, true, &resp);
}

#[test]
fn bad_search_input_is_a_validation_error_envelope() {
    for (kind, q) in [
        (Some("artist".to_string()), "a"),
        (Some("track".to_string()), "valid query"),
        (None, "valid query"),
    ] {
        let resp = block_on(handle_search(&RepoEmpty, &NoCache, kind, q));
        assert_envelope_body("VALIDATION_ERROR", 400, false, &resp);
    }
}

// --- shared contract: lookup, shape, fallback ---------------------------------

fn assert_envelope_shape(env: &ErrorEnvelope, body: &str) {
    let v: serde_json::Value = serde_json::from_str(body).expect("envelope body must be JSON");
    let obj = v.as_object().expect("envelope body must be an object");
    assert_eq!(
        obj.get("code").and_then(|c| c.as_str()),
        Some(env.code),
        "body code must match envelope code"
    );
    assert_eq!(
        obj.get("message").and_then(|m| m.as_str()),
        Some(env.message),
        "body message must match envelope message"
    );
    assert_eq!(
        obj.get("retryable").and_then(|r| r.as_bool()),
        Some(env.retryable),
        "body retryable must match envelope flag"
    );
    assert!(
        !obj.contains_key("error"),
        "legacy {{\"error\": ...}} shape is banned: {body}"
    );
}

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
