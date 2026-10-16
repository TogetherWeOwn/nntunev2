//! Acceptance tests: per-worker structured logging standard with trace IDs.
//!
//! Slice contract for the `stream` worker (paired with the `observability:
//! structured logging standard` build card, which builds on this branch and
//! never edits this file).
//!
//! NOTE: unlike api/ingest/mb, `stream` keeps its legacy stream log shape log shape and
//! only adds `trace_id`. This file therefore asserts the shared schema as a
//! SUPERSET (the six fields must be present; extras from the legacy stream log shape shape
//! are allowed) and accepts the existing stream event vocabulary — the event
//! name only needs to be lowercase `[a-z0-9_.]`, with no `stream.` prefix
//! requirement.
//!
//! ## What the Builder must create to turn these tests GREEN
//! 1. `docs/logging-standard.md` at the repo root, pinning the shared field
//!    schema (`ts`, `level`, `worker`, `trace_id`, `event`, `fields`) plus
//!    the trace-id propagation rule (`x-trace-id`) and the PII/redaction rule.
//! 2. `workers/stream/tests/fixtures/logging_golden.json`: ONE JSON object,
//!    the golden `stream` log line (legacy stream log shape shape + `trace_id`). Example
//!    (trace_id is 32 lowercase hex):
//!    ```json
//!    {
//!      "ts": "2026-10-15T20:00:00.123Z",
//!      "level": "info",
//!      "worker": "stream",
//!      "trace_id": "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c",
//!      "event": "stream.play.start",
//!      "fields": {"track_id": "track-a", "cache_hit": false}
//!    }
//!    ```
//!    Rules: `level` is one of `debug|info|warn|error`; `fields` is a flat
//!    object (string/number/bool/null values only, no nesting) whose keys
//!    never contain secret substrings (password, secret, token, cookie, ...
//!    — see `BANNED_KEY_PARTS`).
//!
//! Static fixtures only: no live Cloudflare tail, no network. std-only so no
//! new dependencies are needed (`nntune-stream` has zero dependencies).

use std::path::PathBuf;

/// Worker under test in this file.
const EXPECTED_WORKER: &str = "stream";
/// Fixture the Builder creates (relative to this package root).
const FIXTURE_REL: &str = "tests/fixtures/logging_golden.json";
/// Standard doc the Builder creates (relative to this package root).
const DOC_REL: &str = "../../docs/logging-standard.md";

const REQUIRED_FIELDS: &[&str] = &["ts", "level", "worker", "trace_id", "event", "fields"];
const ALLOWED_LEVELS: &[&str] = &["debug", "info", "warn", "error"];
/// Key substrings that must never appear in `fields` (case-insensitive).
const BANNED_KEY_PARTS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "api_key",
    "apikey",
    "token",
    "cookie",
    "authorization",
    "private_key",
    "mnemonic",
    "ssn",
    "credit_card",
    "card_number",
    "cvv",
];

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Read an artifact the Builder owns; panic with a RED message if absent.
fn read_artifact(rel: &str, what: &str) -> String {
    let path = manifest_dir().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "RED (expected before the build): missing {what} at {rel} \
             (resolved to {}). The Builder creates it per this file's header contract.",
            path.display()
        )
    })
}

/// True when `"key" :` appears (top-level or nested — caller scopes the input).
fn has_key(json: &str, key: &str) -> bool {
    let needle = format!("\"{key}\"");
    let mut rest = json;
    while let Some(pos) = rest.find(needle.as_str()) {
        let after = &rest[pos + needle.len()..];
        let after = after.trim_start();
        if after.starts_with(':') {
            return true;
        }
        rest = after;
    }
    false
}

/// Extract a top-level string field value (handles `\"` and `\\` escapes).
fn extract_string_field(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let pos = json.find(needle.as_str())?;
    let after = json[pos + needle.len()..].trim_start();
    let after = after.strip_prefix(':')?.trim_start();
    let body = after.strip_prefix('"')?;
    let mut out = String::new();
    let mut esc = false;
    for ch in body.chars() {
        if esc {
            out.push(ch);
            esc = false;
        } else if ch == '\\' {
            esc = true;
        } else if ch == '"' {
            return Some(out);
        } else {
            out.push(ch);
        }
    }
    None
}

/// Extract the inner body of the top-level `"fields": {...}` object.
fn extract_fields_body(json: &str) -> Option<String> {
    let needle = "\"fields\"";
    let pos = json.find(needle)?;
    let after = json[pos + needle.len()..].trim_start();
    let after = after.strip_prefix(':')?.trim_start();
    let start = after.find('{')?;
    let chars: Vec<char> = after[start..].chars().collect();
    let mut depth = 0usize;
    let mut in_str = false;
    let mut esc = false;
    for (i, ch) in chars.iter().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if *ch == '\\' {
                esc = true;
            } else if *ch == '"' {
                in_str = false;
            }
        } else if *ch == '"' {
            in_str = true;
        } else if *ch == '{' {
            depth += 1;
        } else if *ch == '}' {
            depth -= 1;
            if depth == 0 {
                return Some(chars[1..i].iter().collect());
            }
        }
    }
    None
}

fn is_hex32(s: &str) -> bool {
    s.len() == 32
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn is_rfc3339_utc(s: &str) -> bool {
    // Lenient RFC3339 UTC check without extra deps: `YYYY-MM-DDTHH:MM:SS[.frac]Z`.
    if !(s.len() >= 20 && s.ends_with('Z') && s.contains('T') && s.starts_with("20")) {
        return false;
    }
    let core = &s[..10];
    let bytes = core.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    core[..4].parse::<u32>().is_ok() && core[5..7].parse::<u32>().is_ok()
}

/// Stream keeps its legacy stream log shape event vocabulary: lowercase `[a-z0-9_.]`,
/// non-empty, no leading/trailing separator. A dot is NOT required.
fn is_stream_event_name(s: &str) -> bool {
    if s.is_empty() || s.len() > 96 {
        return false;
    }
    if s.starts_with(['.', '_']) || s.ends_with(['.', '_']) {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
}

/// Keys of the flat `fields` object: strings followed by `:`.
fn field_keys(fields_body: &str) -> Vec<String> {
    let chars: Vec<char> = fields_body.chars().collect();
    let mut keys = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '"' {
            i += 1;
            continue;
        }
        let mut key = String::new();
        let mut esc = false;
        let mut j = i + 1;
        let mut closed = false;
        while j < chars.len() {
            let c = chars[j];
            if esc {
                key.push(c);
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                closed = true;
                break;
            } else {
                key.push(c);
            }
            j += 1;
        }
        if !closed {
            break;
        }
        let mut k = j + 1;
        while k < chars.len() && chars[k].is_whitespace() {
            k += 1;
        }
        if k < chars.len() && chars[k] == ':' {
            keys.push(key);
        }
        i = j + 1;
    }
    keys
}

/// First banned key-part found among the `fields` keys (case-insensitive,
/// substring match so `grant_token` and `apiKey` variants are caught).
fn banned_in_fields(fields_body: &str) -> Option<String> {
    for key in field_keys(fields_body) {
        let lower = key.to_lowercase();
        if BANNED_KEY_PARTS.iter().any(|part| lower.contains(part)) {
            return Some(key);
        }
    }
    None
}

/// `fields` values must be flat primitives: no nested objects/arrays outside strings.
fn fields_are_flat(fields_body: &str) -> bool {
    let mut stripped = String::with_capacity(fields_body.len());
    let mut in_str = false;
    let mut esc = false;
    for ch in fields_body.chars() {
        if in_str {
            if esc {
                esc = false;
            } else if ch == '\\' {
                esc = true;
            } else if ch == '"' {
                in_str = false;
            }
        } else if ch == '"' {
            in_str = true;
        } else {
            stripped.push(ch);
        }
    }
    !stripped.contains(['{', '}', '[', ']'])
}

#[test]
fn golden_line_conforms_to_schema() {
    let line = read_artifact(FIXTURE_REL, "golden log-line fixture");
    for field in REQUIRED_FIELDS {
        assert!(
            has_key(&line, field),
            "golden line must carry `{field}`: {line}"
        );
    }
    let level = extract_string_field(&line, "level").expect("level must be a JSON string");
    assert!(
        ALLOWED_LEVELS.contains(&level.as_str()),
        "level must be one of {ALLOWED_LEVELS:?}, got `{level}`"
    );
    let ts = extract_string_field(&line, "ts").expect("ts must be a JSON string");
    assert!(
        is_rfc3339_utc(&ts),
        "ts must be RFC3339 UTC ending in Z, got `{ts}`"
    );
    let worker = extract_string_field(&line, "worker").expect("worker must be a JSON string");
    assert_eq!(
        worker, EXPECTED_WORKER,
        "worker field must be `{EXPECTED_WORKER}`"
    );
    let fields_body = extract_fields_body(&line).expect("fields must be a JSON object");
    assert!(
        fields_are_flat(&fields_body),
        "fields must be flat (no nested objects/arrays): {{{fields_body}}}"
    );
}

#[test]
fn golden_line_has_valid_trace_id_and_event() {
    let line = read_artifact(FIXTURE_REL, "golden log-line fixture");
    let trace_id = extract_string_field(&line, "trace_id").expect("trace_id must be a JSON string");
    assert!(
        is_hex32(&trace_id),
        "trace_id must be 32 lowercase hex chars (128-bit), got `{trace_id}`"
    );
    let event = extract_string_field(&line, "event").expect("event must be a JSON string");
    assert!(
        is_stream_event_name(&event),
        "event must be lowercase [a-z0-9_.], got `{event}`"
    );
}

#[test]
fn golden_line_contains_no_pii_or_secrets() {
    let line = read_artifact(FIXTURE_REL, "golden log-line fixture");
    let fields_body = extract_fields_body(&line).expect("fields must be a JSON object");
    assert!(
        banned_in_fields(&fields_body).is_none(),
        "fields must not carry secret-looking keys: {{{fields_body}}}"
    );
    // The check itself is real: a secret-looking line must be rejected.
    let bad = r#"{"fields": {"user_id": "u1", "password": "s3cret"}}"#;
    let bad_body = extract_fields_body(bad).unwrap();
    assert_eq!(banned_in_fields(&bad_body).as_deref(), Some("password"));
    let bad_token = r#"{"fields": {"grant_token": "tok_abc"}}"#;
    assert!(banned_in_fields(&extract_fields_body(bad_token).unwrap()).is_some());
}

#[test]
fn logging_standard_doc_pins_schema() {
    let doc = read_artifact(DOC_REL, "logging-standard doc");
    for field in REQUIRED_FIELDS {
        assert!(
            doc.contains(field),
            "logging-standard.md must pin the `{field}` field"
        );
    }
    let lower = doc.to_lowercase();
    assert!(
        lower.contains("x-trace-id") || (lower.contains("trace") && lower.contains("propagat")),
        "logging-standard.md must state the trace-id propagation rule"
    );
    assert!(
        lower.contains("redact") || lower.contains("pii") || lower.contains("secret"),
        "logging-standard.md must state the PII/redaction rule"
    );
}
