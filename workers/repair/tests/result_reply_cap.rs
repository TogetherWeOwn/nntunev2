//! RED acceptance harness: check `MAX_RESULT_BYTES` *before*
//! materialising container result replies.
//!
//! Defect (at the source SHA this harness was written against):
//! both reply sites in `workers/repair/src/runtime.rs` collect the
//! full response body first and only then compare its length to
//! [`MAX_RESULT_BYTES`]:
//!
//! - the Worker front door (`fetch`, the Durable Object reply):
//!   `response.bytes().await` matched against
//!   `bytes.len() <= MAX_RESULT_BYTES`;
//! - `container_json` (every Container JSON reply, including the
//!   `/repair` result): `response.bytes().await` followed by
//!   `if bytes.len() > MAX_RESULT_BYTES`.
//!
//! `.bytes()` materialises the whole body unbounded, so an
//! oversized (or endless) reply is fully buffered before the cap
//! ever gets a say — the cap bounds what is *kept*, not what is
//! *read*. The required behaviour is the one `read_capped` already
//! implements for request bodies in the same file: stream the
//! reply and abort as soon as the accumulated length exceeds the
//! cap, so at most `cap + one chunk` is ever materialised. A
//! `Content-Length` pre-check alone is not sufficient (the
//! Container may answer chunked, with no length to check), though
//! it may be added as an extra early reject.
//!
//! The `runtime` module compiles only for `wasm32`, so a native
//! integration target cannot call it; the RED pins below read
//! `runtime.rs` as source text with `include_str!`, after
//! stripping comments and string/char literals (so a comment or
//! string merely *mentioning* `.bytes()` or the cap cannot fake a
//! pass — see `stripper_removes_comments_and_strings`). Whitespace
//! is removed before matching, so formatting choices stay free.
//!
//! What the Builder must do to turn this green (without editing
//! this file): read both replies through a streaming capped read
//! bounded by `MAX_RESULT_BYTES` (e.g. a `read_response_capped`
//! sibling of `read_capped`, or an inline `.stream()` loop),
//! keeping the existing fault mapping (front door: 503
//! `repair_unavailable`; `container_json`: 502
//! `container_unavailable`) and the boundary semantics pinned by
//! the green controls (exactly-at-cap admitted, one-over
//! rejected).
//!
//! The two remaining legitimate `.bytes()` calls are out of
//! scope and must stay allowed: the discarded `stage_input`
//! acknowledgement body (no cap comparison; not a result reply)
//! and the R2 object body in `read_object_capped`, whose size is
//! checked against R2 metadata *before* materialisation.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked -p nntune-repair --test result_reply_cap
//! ```

use nntune_core::par2::scratch::object_key_for_file_name;
use nntune_repair::contract::{
    classify_container_reply, ingest_reply_body, ContainerOutcome, OutputFile, RepairResult,
    RepairStatus, MAX_RESULT_BYTES,
};

/// `runtime.rs` source, read as text (NOT a crate import — the
/// module is `cfg(target_arch = "wasm32")` and unreachable from
/// native tests).
const RUNTIME_RS: &str = include_str!("../src/runtime.rs");

// ---------------------------------------------------------------------------
// Source preparation: strip comments and literals, slice reply regions.
// ---------------------------------------------------------------------------

/// Replace every comment and string/char literal in Rust source
/// with spaces (newlines preserved), so token searches only see
/// real code. Handles line/block (nested) comments, escaped
/// string and char literals, raw strings, byte strings/chars, and
/// lifetimes (a `'` that is not a char literal is left alone).
fn strip_comments_and_literals(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    let blank = |out: &mut Vec<u8>, b: u8| out.push(if b == b'\n' { b'\n' } else { b' ' });
    while i < bytes.len() {
        let b = bytes[i];
        let next = bytes.get(i + 1).copied();
        // Raw string: r"..." / r#"..."# / br#"..."# (not an ident).
        let raw_hashes = raw_string_hashes(bytes, i);
        if let Some(hashes) = raw_hashes {
            // Blank the opener (`r`/`br` + hashes + quote).
            while i < bytes.len() && bytes[i] != b'"' {
                blank(&mut out, bytes[i]);
                i += 1;
            }
            if i < bytes.len() {
                blank(&mut out, bytes[i]); // opening quote
                i += 1;
            }
            // Blank until quote followed by exactly `hashes` '#'.
            while i < bytes.len() {
                if bytes[i] == b'"'
                    && (0..hashes).all(|k| bytes.get(i + 1 + k) == Some(&b'#'))
                    && bytes.get(i + 1 + hashes) != Some(&b'#')
                {
                    blank(&mut out, b'"');
                    i += 1;
                    for _ in 0..hashes {
                        blank(&mut out, b'#');
                        i += 1;
                    }
                    break;
                }
                blank(&mut out, bytes[i]);
                i += 1;
            }
            continue;
        }
        match (b, next) {
            (b'/', Some(b'/')) => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    blank(&mut out, bytes[i]);
                    i += 1;
                }
            }
            (b'/', Some(b'*')) => {
                let mut depth = 0usize;
                while i < bytes.len() {
                    if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        blank(&mut out, bytes[i]);
                        blank(&mut out, bytes[i + 1]);
                        i += 2;
                    } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        blank(&mut out, bytes[i]);
                        blank(&mut out, bytes[i + 1]);
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        blank(&mut out, bytes[i]);
                        i += 1;
                    }
                }
            }
            (b'"', _) => {
                blank(&mut out, b);
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => {
                            blank(&mut out, bytes[i]);
                            i += 1;
                            if i < bytes.len() {
                                blank(&mut out, bytes[i]);
                                i += 1;
                            }
                        }
                        b'"' => {
                            blank(&mut out, b'"');
                            i += 1;
                            break;
                        }
                        other => {
                            blank(&mut out, other);
                            i += 1;
                        }
                    }
                }
            }
            (b'\'', _) if is_char_literal(bytes, i) => {
                blank(&mut out, b);
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => {
                            blank(&mut out, bytes[i]);
                            i += 1;
                            if i < bytes.len() {
                                blank(&mut out, bytes[i]);
                                i += 1;
                            }
                        }
                        b'\'' => {
                            blank(&mut out, b'\'');
                            i += 1;
                            break;
                        }
                        other => {
                            blank(&mut out, other);
                            i += 1;
                        }
                    }
                }
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).expect("blanking only ASCII keeps UTF-8 valid here")
}

/// If a raw string opener (`r"`, `r#"`, `br"`, `br#"`, …) starts at
/// `i`, return its hash count. The `r` must not be preceded by an
/// identifier character (otherwise it is part of an ident).
fn raw_string_hashes(bytes: &[u8], i: usize) -> Option<usize> {
    let mut j = i;
    if bytes[j] == b'b' {
        j += 1;
    }
    if bytes.get(j) != Some(&b'r') {
        return None;
    }
    if i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_') {
        return None;
    }
    j += 1;
    let mut hashes = 0usize;
    while bytes.get(j) == Some(&b'#') {
        hashes += 1;
        j += 1;
    }
    (bytes.get(j) == Some(&b'"')).then_some(hashes)
}

/// A `'` opens a char literal when it is `'\…'` (escape) or `'.'`
/// (single char, closing quote two bytes later — multi-byte UTF-8
/// chars excepted, none occur as char literals in `runtime.rs`).
/// Anything else is a lifetime.
fn is_char_literal(bytes: &[u8], i: usize) -> bool {
    matches!(
        (bytes.get(i + 1), bytes.get(i + 2)),
        (Some(b'\\'), _) | (Some(_), Some(b'\''))
    )
}

/// Code-only view of `runtime.rs` (comments/literals blanked).
fn runtime_code() -> String {
    strip_comments_and_literals(RUNTIME_RS)
}

/// Remove all whitespace so matching is formatting-insensitive.
fn compact(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Slice `src` from the first `start` marker to the next `end`
/// marker after it. Panics with a harness-oriented message when a
/// marker is missing (the regions anchor on items the fix must
/// keep: the front-door handler, the DO struct, `container_json`
/// and `read_object_capped`).
fn region<'a>(src: &'a str, start: &str, end: &str, what: &str) -> &'a str {
    let from = src
        .find(start)
        .unwrap_or_else(|| panic!("harness anchor missing: {what} must contain `{start}`"));
    let rest = &src[from..];
    let to = rest
        .find(end)
        .unwrap_or_else(|| panic!("harness anchor missing: {what} must contain `{end}`"));
    &rest[..to]
}

/// The Worker front-door handler: from `#[event(fetch)]` to the
/// Durable Object struct that follows it.
fn front_door_region(src: &str) -> &str {
    region(
        src,
        "#[event(fetch)]",
        "pub struct RepairContainer",
        "front door",
    )
}

/// `container_json`: from its signature to the next helper.
fn container_json_region(src: &str) -> &str {
    region(
        src,
        "async fn container_json",
        "async fn read_object_capped",
        "container_json",
    )
}

/// Shared RED assertion for one reply site: the body must be read
/// through the cap (passed as an argument to a capped reader, or
/// streamed inline), never collected unbounded first.
fn assert_reply_read_through_cap(code_region: &str, site: &str) {
    let c = compact(code_region);
    assert!(
        !c.contains(".bytes()") && !c.contains(".text()"),
        "{site}: reply body must not be collected with an unbounded \
         whole-body read (`.bytes()`/`.text()`) before the \
         MAX_RESULT_BYTES check — stream it through a capped read instead \
         (see `read_capped` in the same file)"
    );
    assert!(
        c.contains("MAX_RESULT_BYTES)")
            || c.contains("MAX_RESULT_BYTES,")
            || c.contains(".stream()"),
        "{site}: MAX_RESULT_BYTES must bound the read itself — pass it to \
         the capped reader (or stream inline against it), not compare \
         after the full body is already materialised"
    );
}

// ---------------------------------------------------------------------------
// RED pins: each fails at the source SHA for the stated reason.
// ---------------------------------------------------------------------------

#[test]
fn worker_fetch_does_not_collect_do_reply_before_cap() {
    // At the source SHA: `response.bytes().await` materialises the
    // whole DO reply, then the match guard compares
    // `bytes.len() <= MAX_RESULT_BYTES`.
    let code = runtime_code();
    assert_reply_read_through_cap(front_door_region(&code), "Worker fetch (DO reply)");
}

#[test]
fn container_json_does_not_collect_container_reply_before_cap() {
    // At the source SHA: `response.bytes().await` materialises the
    // whole Container reply, then `bytes.len() > MAX_RESULT_BYTES`
    // rejects — after the memory is already spent.
    let code = runtime_code();
    assert_reply_read_through_cap(
        container_json_region(&code),
        "container_json (Container reply)",
    );
}

#[test]
fn no_unbounded_collect_remains_except_prechecked_sites() {
    // Sham-fix guard: routing the two reply sites through a helper
    // that still calls `.bytes()`/`.text()` internally keeps the
    // global count red. Exactly two `.bytes()` calls may remain:
    // the discarded `stage_input` acknowledgement and the R2 body
    // in `read_object_capped` (size pre-checked from metadata).
    let c = compact(&runtime_code());
    let bytes_calls = c.matches(".bytes()").count();
    assert!(
        bytes_calls <= 2,
        "runtime.rs must keep at most the 2 pre-checked/non-result \
         `.bytes()` sites (stage_input discard, read_object_capped R2 \
         body); found {bytes_calls} whole-body collects in code — a \
         reply site (or its helper) still materialises before the cap"
    );
    assert!(
        !c.contains(".text()"),
        "runtime.rs must not swap `.bytes()` for an equally unbounded \
         `.text()` collect on a reply body"
    );
}

// ---------------------------------------------------------------------------
// Green controls: adjacent behaviour that must stay green.
// ---------------------------------------------------------------------------

#[test]
fn cap_value_is_one_mebibyte() {
    assert_eq!(MAX_RESULT_BYTES, 1 << 20);
}

fn valid_result_bytes() -> Vec<u8> {
    let result = RepairResult {
        status: RepairStatus::Repaired,
        exit_code: Some(0),
        files: vec![OutputFile {
            file_name: "track.flac".to_string(),
            key: object_key_for_file_name("track.flac"),
            size: 7,
        }],
        manifest_digest: String::new(),
    };
    serde_json::to_vec(&result).unwrap()
}

/// Pad a valid JSON body with trailing whitespace (insignificant
/// to the parser) to an exact total length, so only the size gate
/// can distinguish at-cap from over-cap fixtures.
fn padded_to(base: &[u8], target_len: usize) -> Vec<u8> {
    assert!(
        base.len() <= target_len,
        "base body is {} bytes, over target {target_len}",
        base.len()
    );
    let mut body = base.to_vec();
    body.resize(target_len, b' ');
    body
}

#[test]
fn classify_admits_valid_result_exactly_at_cap() {
    // Boundary control: moving the check earlier must not turn
    // `>` into `>=`. Exactly-at-cap stays admissible at the
    // classification layer that `container_json` feeds.
    let body = padded_to(&valid_result_bytes(), MAX_RESULT_BYTES);
    assert_eq!(body.len(), MAX_RESULT_BYTES);
    match classify_container_reply(200, &body) {
        ContainerOutcome::Done(result) => {
            assert_eq!(result.status, RepairStatus::Repaired);
            assert_eq!(result.files.len(), 1);
        }
        other => panic!("expected Done for exactly-at-cap valid result, got {other:?}"),
    }
}

#[test]
fn classify_rejects_valid_result_one_byte_over_cap_as_502() {
    // Still valid JSON after padding, so only the size gate can
    // reject it; the rejection stays the retryable 502.
    let body = padded_to(&valid_result_bytes(), MAX_RESULT_BYTES + 1);
    assert_eq!(body.len(), MAX_RESULT_BYTES + 1);
    serde_json::from_slice::<RepairResult>(&body).unwrap();
    assert_eq!(
        classify_container_reply(200, &body),
        ContainerOutcome::Forward(502, "container_unavailable".to_string())
    );
}

#[test]
fn classify_forwards_known_container_error_code() {
    // Adjacent behaviour the capped read must not disturb: small
    // non-200 replies keep forwarding their stable code.
    let body = br#"{"error":"invalid_manifest"}"#;
    assert!(body.len() < MAX_RESULT_BYTES);
    assert_eq!(
        classify_container_reply(400, body),
        ContainerOutcome::Forward(400, "invalid_manifest".to_string())
    );
}

#[test]
fn ingest_reply_body_stays_small() {
    // The front door forwards the DO's reply to ingest; that
    // reply is the small status document, far under the cap the
    // front-door read will enforce.
    let result: RepairResult = serde_json::from_slice(&valid_result_bytes()).unwrap();
    let body = ingest_reply_body(&result);
    assert!(
        body.len() < MAX_RESULT_BYTES,
        "ingest reply must stay under MAX_RESULT_BYTES, got {} bytes",
        body.len()
    );
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["status"], "repaired");
    assert_eq!(parsed["file_count"], 1);
}

#[test]
fn reply_sites_keep_existing_fault_mapping_literals() {
    // Raw-source control (literals are the contract here, so no
    // stripping): an over-cap or unreadable reply keeps mapping
    // to the same faults as at the source SHA — 503
    // `repair_unavailable` at the front door, 502
    // `container_unavailable` in `container_json`.
    let front = front_door_region(RUNTIME_RS);
    assert!(
        front.contains("503") && front.contains("\"repair_unavailable\""),
        "front door must keep mapping a bad/oversized DO reply to 503 \
         repair_unavailable"
    );
    let json = container_json_region(RUNTIME_RS);
    assert!(
        json.contains("502") && json.contains("\"container_unavailable\""),
        "container_json must keep mapping a bad/oversized Container reply \
         to 502 container_unavailable"
    );
}

#[test]
fn stripper_removes_comments_and_strings() {
    // Sensitivity control for the harness itself: `.bytes()`
    // mentioned in a comment, a string, or a raw string must not
    // count as a whole-body collect, and real code must survive.
    let sample = "let a = read_capped(x); // response.bytes().await\n\
                  let s = \".bytes()\"; /* .bytes() */ let r = r#\".bytes()\"#;\n\
                  let life: &'static str = s; real_call();";
    let stripped = compact(&strip_comments_and_literals(sample));
    assert!(
        !stripped.contains(".bytes()"),
        "comment/string mentions of .bytes() must be stripped, got: {stripped}"
    );
    assert!(
        stripped.contains("read_capped(x)") && stripped.contains("real_call()"),
        "real code must survive stripping, got: {stripped}"
    );
    assert!(
        stripped.contains("&'staticstr"),
        "lifetimes must not be mistaken for char literals, got: {stripped}"
    );
}
