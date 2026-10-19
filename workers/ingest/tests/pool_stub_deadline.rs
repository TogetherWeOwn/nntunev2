//! Pool stub calls abort on a deadline instead of hanging a delivery.
//!
//! Gap under test (`workers/ingest/src/pool_probe.rs`, `call_pool_probe`;
//! `workers/ingest/src/pool_body.rs`, `call_pool_body`): each awaits the pool
//! stub `fetch_with_request` and then the full body with no per-call
//! deadline. The batch driver only checks its wall-time budget between
//! batches (and only after the first answer folds), so one hung stub call
//! never returns and holds the delivery.
//!
//! Contract the implementation must provide (source-pinned, no live stub):
//!
//! - `call_pool_probe` races its stub fetch against a bounded `Delay`
//!   (the worker runtime timer, armed before the fetch like the existing
//!   MusicBrainz body path) and expiry maps to the existing
//!   `PoolProbeError::Retry`.
//! - `call_pool_body` races the same way and expiry maps to the existing
//!   `PoolBodyError::Retry`.
//! - No new error variant is introduced for the timeout: both error enums
//!   keep exactly `Retry` and `Rejected`.
//! - Comment text never satisfies the pin: stripping `//` and `/* */`
//!   comments (string-aware, so `https://` inside literals survives) must
//!   still leave the `Delay` bound visible. A comment that merely mentions
//!   the timer must not turn these tests green.
//!
//! Deterministic and fast: every test below is a string assertion over the
//! production source plus synchronous `classify_*` controls. Nothing awaits
//! a stub, nothing sleeps, and the suite never hangs the test process.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked -p nntune-ingest --test pool_stub_deadline
//! ```

use nntune_ingest::pool_body::{classify_body_reply, BodyFetch, PoolBodyError};
use nntune_ingest::pool_probe::{classify_reply, PoolProbeError};
use nntune_ingest::probe::{ProbeOutcome, ProbeRequest};

// ---------------------------------------------------------------------------
// Production source helpers (pre-test source only, comments stripped).
// ---------------------------------------------------------------------------

/// Production source before the unit-test module, so the file's own test
/// literals cannot satisfy the wiring pins.
fn production_part(source: &str) -> &str {
    source
        .split_once("#[cfg(test)]")
        .map(|(head, _)| head)
        .unwrap_or(source)
}

fn probe_source() -> &'static str {
    include_str!("../src/pool_probe.rs")
}

fn body_source() -> &'static str {
    include_str!("../src/pool_body.rs")
}

/// Strip `//` line comments and `/* */` (nested) block comments, string
/// aware so `https://` inside `"..."`, `b"..."`, `r"..."` and `br#"..."#`
/// literals survives. Block comments are replaced with a single space so
/// surrounding tokens do not fuse.
fn strip_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < bytes.len() {
        // Inside check helpers need the raw byte slice; handle raw-string
        // starts first (optional `b`, then `r`, then `#*`, then `"`).
        if let Some((hashes, prefix_len)) = raw_string_prefix(source, i) {
            // Copy the prefix (`br#"`, `r"`, ...) verbatim, then copy the
            // raw body until the matching close (`"` + same hashes).
            out.push_str(&source[i..i + prefix_len]);
            i += prefix_len;
            let close = format!("\"{}", "#".repeat(hashes));
            if let Some(end) = source[i..].find(&close) {
                let body_end = i + end + close.len();
                out.push_str(&source[i..body_end]);
                i = body_end;
            } else {
                // Unterminated: copy the rest and stop.
                out.push_str(&source[i..]);
                break;
            }
            continue;
        }
        // Line comment `//` (covers `///` and `//!` doc comments).
        if bytes[i] == b'/' && i + 1 < bytes.len() && (bytes[i + 1] == b'/' || bytes[i + 1] == b'*')
        {
            if bytes[i + 1] == b'/' {
                // Skip to (not past) the newline; keep the newline so line
                // structure survives.
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                if i < bytes.len() {
                    out.push('\n');
                    i += 1;
                }
                continue;
            }
            // Block comment `/*`, nest-aware.
            let mut depth = 1u32;
            i += 2;
            while i < bytes.len() && depth > 0 {
                if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if bytes[i] == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            out.push(' ');
            continue;
        }
        // Byte-string or plain-string start: `b"` was not a raw string, so
        // a bare `"` (optionally right after a `b` already copied) opens a
        // cooked string. Copy it verbatim with escapes.
        if bytes[i] == b'"' {
            out.push('"');
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    out.push('\\');
                    out.push(bytes[i + 1] as char);
                    i += 2;
                } else if bytes[i] == b'"' {
                    out.push('"');
                    i += 1;
                    break;
                } else {
                    out.push(bytes[i] as char);
                    i += 1;
                }
            }
            continue;
        }
        // A lone `b` directly before a `"` opens a byte string: keep the
        // `b` and let the next iteration open the string. Detect here only
        // to avoid treating the `b` as ordinary text when it prefixes a
        // string containing `//`.
        if bytes[i] == b'b' && i + 1 < bytes.len() && bytes[i + 1] == b'"' && at_boundary(source, i)
        {
            out.push('b');
            i += 1;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// If `source[i..]` opens a raw string (`r"`, `r#"..."#`, `br"`, `br##"...`),
/// return `(hash_count, prefix_len)`. `r` must sit on a token boundary so
/// identifiers ending in `r` (e.g. `error`) never trigger.
fn raw_string_prefix(source: &str, i: usize) -> Option<(usize, usize)> {
    let rest = &source[i..];
    let bytes = rest.as_bytes();
    let mut j = 0;
    // Optional `b` prefix.
    if !bytes.is_empty() && bytes[0] == b'b' {
        // `b` must be a boundary and must be followed by `r`.
        if !at_boundary(source, i) {
            return None;
        }
        if bytes.len() < 2 || bytes[1] != b'r' {
            return None;
        }
        j = 1;
    }
    // Required `r` (boundary-checked when there is no `b` prefix).
    if j == 0 {
        if bytes.is_empty() || bytes[0] != b'r' || !at_boundary(source, i) {
            return None;
        }
    } else if bytes[j] != b'r' {
        return None;
    }
    j += 1;
    let mut hashes = 0;
    while j < bytes.len() && bytes[j] == b'#' {
        hashes += 1;
        j += 1;
    }
    if j < bytes.len() && bytes[j] == b'"' {
        j += 1;
        Some((hashes, j))
    } else {
        None
    }
}

/// Token boundary before `source[i]`: start of file or the previous char is
/// not an identifier char (so the `r` in `error` never opens a raw string).
fn at_boundary(source: &str, i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let prev = source.as_bytes()[i - 1];
    !(prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'#')
}

/// Code of one async stub call: from its `async fn <name>` marker to the end
/// of the pre-test production part. Both stub calls are the last item before
/// the test module (probe) or the end of the file (body), so this slice is
/// exactly the function under test.
fn call_slice(source: &str, marker: &str) -> String {
    let prod = production_part(source);
    prod.split_once(marker)
        .unwrap_or_else(|| panic!("{marker} must exist"))
        .1
        .to_string()
}

fn probe_call_code() -> String {
    strip_comments(&call_slice(probe_source(), "async fn call_pool_probe"))
}

fn body_call_code() -> String {
    strip_comments(&call_slice(body_source(), "async fn call_pool_body"))
}

/// Variant names of `pub enum <name>` in comment-stripped source, in
/// declaration order. Tracks brace/paren/bracket depth so fields inside
/// `Retry { ... }` never read as variants.
fn enum_variants(stripped: &str, name: &str) -> Vec<String> {
    let marker = format!("pub enum {name}");
    let after = stripped
        .split_once(&marker)
        .unwrap_or_else(|| panic!("{marker} must exist"))
        .1;
    let open = after.find('{').expect("enum body must open");
    let mut depth_brace = 1i32;
    let mut depth_paren = 0i32;
    let mut depth_bracket = 0i32;
    let mut variants = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    // Walk from the opening brace; collect top-level capitalized words.
    let body = &after[open..];
    let mut chars = body.chars().peekable();
    // Skip the opening brace itself (depth already accounts for it).
    let _ = chars.next();
    let mut token = String::new();
    macro_rules! flush_token {
        () => {
            if !token.is_empty()
                && token.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && depth_brace == 1
                && depth_paren == 0
                && depth_bracket == 0
            {
                variants.push(std::mem::take(&mut token));
            } else {
                token.clear();
            }
        };
    }
    let _ = (&mut current, &mut in_word);
    for ch in chars {
        match ch {
            '{' if depth_paren == 0 && depth_bracket == 0 => {
                flush_token!();
                depth_brace += 1;
            }
            '}' => {
                flush_token!();
                depth_brace -= 1;
                if depth_brace == 0 {
                    break;
                }
            }
            '(' if depth_brace == 1 && depth_bracket == 0 => {
                flush_token!();
                depth_paren += 1;
            }
            ')' if depth_paren > 0 => {
                depth_paren -= 1;
            }
            '[' if depth_brace == 1 && depth_paren == 0 => {
                flush_token!();
                depth_bracket += 1;
            }
            ']' if depth_bracket > 0 => {
                depth_bracket -= 1;
            }
            ',' | ';' | '\n' | ' ' | '\t' | '\r' | ':' | '=' | '<' | '>' | '|' | '&' | '#'
            | '!' | '"' | '\'' | '/' | '*' | '.' | '-' | '+' => {
                flush_token!();
            }
            _ => {
                if depth_brace == 1 && depth_paren == 0 && depth_bracket == 0 {
                    if token.is_empty() {
                        // Start a new token only on identifier chars.
                        if ch.is_ascii_alphanumeric() || ch == '_' {
                            token.push(ch);
                        }
                    } else if ch.is_ascii_alphanumeric() || ch == '_' {
                        token.push(ch);
                    } else {
                        flush_token!();
                    }
                }
            }
        }
    }
    // Drop non-variant capitalized words that can appear at the top level
    // of an enum body (there are none today: only variant names sit at
    // depth 1). Keep the filter strict: exactly Retry + Rejected.
    variants.retain(|v| {
        v.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    });
    variants
}

// ---------------------------------------------------------------------------
// RED pins: the stub calls must race a bounded timer (fail today).
// ---------------------------------------------------------------------------

#[test]
fn probe_stub_call_is_bounded_by_a_deadline() {
    let code = probe_call_code();
    assert!(
        code.contains("fetch_with_request"),
        "slice must cover the pool stub fetch in call_pool_probe"
    );
    assert!(
        code.contains("Delay"),
        "call_pool_probe awaits the pool stub with no per-call deadline: \
         race fetch_with_request against a bounded Delay so a hung first call \
         returns, mapping expiry to PoolProbeError::Retry"
    );
    assert!(
        code.contains("Retry"),
        "a bounded probe call must map timer expiry to the existing PoolProbeError::Retry"
    );
}

#[test]
fn body_stub_call_is_bounded_by_a_deadline() {
    let code = body_call_code();
    assert!(
        code.contains("fetch_with_request"),
        "slice must cover the pool stub fetch in call_pool_body"
    );
    assert!(
        code.contains("Delay"),
        "call_pool_body awaits the pool stub with no per-call deadline: \
         race fetch_with_request against a bounded Delay so a hung first call \
         returns, mapping expiry to PoolBodyError::Retry"
    );
    assert!(
        code.contains("Retry"),
        "a bounded body call must map timer expiry to the existing PoolBodyError::Retry"
    );
}

#[test]
fn expiry_uses_the_existing_retry_variants_only() {
    // A timeout is a retryable stall, not a contract bug: expiry must reuse
    // `Retry { .. }`. Requiring exactly these two variants makes any new
    // timeout variant fail here instead of compiling through.
    let probe_variants = enum_variants(
        &strip_comments(production_part(probe_source())),
        "PoolProbeError",
    );
    assert_eq!(
        probe_variants,
        vec!["Retry".to_string(), "Rejected".to_string()],
        "PoolProbeError must stay exactly Retry/Rejected so deadline expiry reuses Retry"
    );
    let body_variants = enum_variants(
        &strip_comments(production_part(body_source())),
        "PoolBodyError",
    );
    assert_eq!(
        body_variants,
        vec!["Retry".to_string(), "Rejected".to_string()],
        "PoolBodyError must stay exactly Retry/Rejected so deadline expiry reuses Retry"
    );
    // The existing Retry shape the deadline maps to (construction proves the
    // variant stays usable for expiry; both files keep their own constant).
    assert_eq!(
        PoolProbeError::Retry { after_secs: 5 },
        PoolProbeError::Retry { after_secs: 5 }
    );
    assert_eq!(
        PoolBodyError::Retry { after_secs: 5 },
        PoolBodyError::Retry { after_secs: 5 }
    );
    assert_ne!(
        PoolProbeError::Retry { after_secs: 5 },
        PoolProbeError::Rejected
    );
    assert_ne!(
        PoolBodyError::Retry { after_secs: 5 },
        PoolBodyError::Rejected
    );
}

// ---------------------------------------------------------------------------
// Comment-ignoring control (stays green).
// ---------------------------------------------------------------------------

#[test]
fn comment_mentioning_the_timer_does_not_satisfy_the_pin() {
    // A comment that merely names the timer must not count as bounding the
    // call: the pin reads comment-stripped code only.
    let snippet = r#"
        // TODO: bound this with Delay one day
        /* Delay would go here */
        /// Delay in docs still counts as a comment.
        let request = make_request();
    "#;
    let stripped = strip_comments(snippet);
    assert!(
        !stripped.contains("Delay"),
        "comment text must not satisfy the deadline pin, got: {stripped:?}"
    );
    let with_code = format!(
        "{snippet}\nlet _timer = worker::Delay::from(core::time::Duration::from_millis(1));"
    );
    assert!(
        strip_comments(&with_code).contains("Delay"),
        "real timer code must still satisfy the pin after stripping"
    );
    // String literals survive stripping: the placeholder stub URL contains
    // `//` but is code, not a comment.
    let url_code = r#"let url = "https://nntp-pool/probe"; // Delay in comment"#;
    let stripped_url = strip_comments(url_code);
    assert!(
        stripped_url.contains("https://nntp-pool/probe"),
        "string literal URLs must survive comment stripping, got: {stripped_url:?}"
    );
    assert!(
        !stripped_url.contains("Delay"),
        "trailing comment Delay must not leak through, got: {stripped_url:?}"
    );
}

// ---------------------------------------------------------------------------
// Fast behavior controls (stay green, never hang).
// ---------------------------------------------------------------------------

fn probe_request() -> ProbeRequest {
    ProbeRequest {
        job_id: "job-1".to_owned(),
        user_id: "user-1".to_owned(),
        idempotency_key: "key-1".to_owned(),
        batch_index: 0,
        segment_ids: vec![
            "seg-a@synthetic.invalid".to_owned(),
            "seg-b@synthetic.invalid".to_owned(),
        ],
        segment_bytes: vec![None, None],
    }
}

fn probe_ok_body(req: &ProbeRequest) -> String {
    serde_json::json!({
        "batch_index": req.batch_index,
        "present": [req.segment_ids[0]],
        "missing": [req.segment_ids[1]],
    })
    .to_string()
}

#[test]
fn fast_success_control_stays_green() {
    // Synchronous classify-only controls: no stub, no clock, no hang.
    let req = probe_request();
    let outcome: ProbeOutcome =
        classify_reply(&req, 200, None, &probe_ok_body(&req)).expect("valid probe reply");
    assert_eq!(outcome.batch_index, req.batch_index);
    assert_eq!(outcome.present, vec![req.segment_ids[0].clone()]);
    assert_eq!(
        classify_body_reply(200, None, b"article-bytes"),
        Ok(BodyFetch::Bytes(b"article-bytes".to_vec()))
    );
    assert_eq!(
        classify_body_reply(404, None, br#"{"error":"missing"}"#),
        Ok(BodyFetch::Missing)
    );
}

#[test]
fn fast_failure_control_stays_green() {
    let req = probe_request();
    assert!(matches!(
        classify_reply(&req, 503, None, r#"{"error":"busy"}"#),
        Err(PoolProbeError::Retry { .. })
    ));
    assert_eq!(
        classify_reply(&req, 400, None, r#"{"error":"bad"}"#),
        Err(PoolProbeError::Rejected)
    );
    assert!(matches!(
        classify_body_reply(503, None, b"busy"),
        Err(PoolBodyError::Retry { .. })
    ));
    assert_eq!(
        classify_body_reply(400, None, br#"{"error":"bad"}"#),
        Err(PoolBodyError::Rejected)
    );
}
