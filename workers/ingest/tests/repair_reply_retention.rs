//! Repair-reply retention RED pin (tests-only).
//!
//! Contract under test (`workers/ingest/src/repair.rs:29,83,105,141`):
//!
//! - `MAX_REPAIR_REPLY_BYTES == 64 << 10` (65,536 bytes) is enforced as
//!   `body.len() > MAX_REPAIR_REPLY_BYTES` fail-closed to `Retry` before any
//!   JSON parse in `classify_repair_reply`;
//! - `call_repair` must NOT collect the full reply body into memory before
//!   that cap (today it does `response.bytes().await` at `:141` and hands the
//!   unbounded buffer to `classify_repair_reply`);
//! - a 200 reply with a well-formed `{"status":"repaired",...}` body of
//!   exactly 65,536 bytes classifies `RepairCall::Repaired`;
//! - the same shape at 65,537 bytes classifies `RepairCall::Retry`, decided
//!   from the byte length, not from parsing a truncated document.
//!
//! `call_repair` takes a live Worker `Env` (no native constructor), so the
//! retention gate is pinned by production source text (sliced to the
//! pre-`#[cfg(test)]` half, comments stripped) while every classification
//! assertion goes through the public surface: `classify_repair_reply` and
//! `RepairCall`. This file's own literals can never satisfy the source pins
//! because the pins read `../src/repair.rs`, never this file.
//!
//! Sensitivity (implementation stays untouched by this card):
//!
//! - removing the unbounded whole-body collect from `call_repair` (streaming
//!   the reply within `MAX_REPAIR_REPLY_BYTES`) turns
//!   `call_repair_streams_reply_within_cap` green;
//! - adding, removing, or rewording a comment that mentions the whole-body
//!   read changes nothing: the pin strips comments before matching, so only
//!   real code flips it;
//! - raising/lowering the ceiling flips the classifier controls (verified by
//!   the pinned TOG-30679 suite, which this file does not touch);
//! - moving the length check after the JSON parse lets the 65,537-byte
//!   well-formed repaired reply through, failing
//!   `ceiling_plus_one_repaired_is_retry_from_length`.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked -p nntune-ingest --test repair_reply_retention
//! ```

use nntune_ingest::repair::{classify_repair_reply, RepairCall};

/// The crate source under test. Sliced to the pre-`#[cfg(test)]` production
/// half per assertion so this file's own literals cannot satisfy the pins.
const REPAIR_RS: &str = include_str!("../src/repair.rs");

/// Byte ceiling under test: `MAX_REPAIR_REPLY_BYTES == 64 << 10`.
const CEILING_BYTES: usize = 64 << 10;

/// Production source before the unit-test module: pins below must match the
/// shipped code, never this file's own text.
fn production_source() -> &'static str {
    REPAIR_RS
        .split_once("#[cfg(test)]")
        .expect("test module must exist")
        .0
}

/// Slice of one function body out of the production source, bounded by the
/// next marker when present (so pins cannot match a neighboring function).
/// When the function is the last production item, the slice runs to the end
/// of the production source.
fn fn_body(source: &str, name: &str, next: &str) -> String {
    let after = source
        .split_once(name)
        .unwrap_or_else(|| panic!("{name} must exist"))
        .1;
    after
        .split_once(next)
        .map(|(head, _)| head.to_string())
        .unwrap_or_else(|| after.to_string())
}

/// Strip `//` line comments and `/* */` block comments from Rust source,
/// respecting normal/char literals and `r#"`/`br#"` raw strings, so a comment
/// that merely mentions a whole-body read can neither satisfy nor dodge the
/// retention pin: only real code is matched.
fn strip_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < bytes.len() {
        // Raw byte/string literal: `br#"..."#`, `r#"..."#`, `br"..."`, etc.
        if (bytes[i] == b'r' || (bytes[i] == b'b' && i + 1 < bytes.len() && bytes[i + 1] == b'r'))
            && i + 1 < bytes.len()
        {
            let mut j = i;
            if bytes[j] == b'b' {
                j += 1;
            }
            // Now at `r`.
            if j < bytes.len() && bytes[j] == b'r' {
                j += 1;
                let mut hashes = 0usize;
                while j < bytes.len() && bytes[j] == b'#' {
                    hashes += 1;
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'"' {
                    // Copy the whole raw literal verbatim.
                    let closing: Vec<u8> = std::iter::once(b'"')
                        .chain(std::iter::repeat(b'#').take(hashes))
                        .collect();
                    out.push_str(&source[i..=j]);
                    i = j + 1;
                    loop {
                        if i >= bytes.len() {
                            break;
                        }
                        if source[i..].as_bytes().starts_with(&closing) {
                            out.push_str(&source[i..i + closing.len()]);
                            i += closing.len();
                            break;
                        }
                        out.push(bytes[i] as char);
                        i += 1;
                    }
                    continue;
                }
            }
        }
        // Normal string or char literal: copy verbatim (handles `\"`, `\'`,
        // and keeps `https://...` intact so its `//` never starts a comment).
        if bytes[i] == b'"' || bytes[i] == b'\'' {
            let quote = bytes[i];
            out.push(quote as char);
            i += 1;
            while i < bytes.len() {
                let c = bytes[i];
                out.push(c as char);
                if c == b'\\' && i + 1 < bytes.len() {
                    out.push(bytes[i + 1] as char);
                    i += 2;
                    continue;
                }
                if c == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        // Line comment: skip to (not including) the newline.
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // Block comment: skip to after `*/` (no nesting, like rustc).
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Build a well-formed `{"status":"repaired","pad":"<pad>"}` body of exactly
/// `total_len` bytes. Padding lives inside the `pad` JSON string value (plain
/// `x` bytes, no escaping), so the body stays well-formed at any size and the
/// only variable the classifier may react to is the byte length.
fn repaired_body(total_len: usize) -> Vec<u8> {
    const PREFIX: &[u8] = br#"{"status":"repaired","pad":""#;
    const SUFFIX: &[u8] = br#""}"#;
    assert!(
        total_len >= PREFIX.len() + SUFFIX.len(),
        "body must fit the repaired envelope"
    );
    let pad_len = total_len - PREFIX.len() - SUFFIX.len();
    let mut body = Vec::with_capacity(total_len);
    body.extend_from_slice(PREFIX);
    body.extend_from_slice(&vec![b'x'; pad_len]);
    body.extend_from_slice(SUFFIX);
    body
}

#[test]
fn call_repair_streams_reply_within_cap() {
    // RED today: `call_repair` (repair.rs:141) collects the full reply with
    // an unbounded whole-body read and only then applies the byte ceiling
    // inside `classify_repair_reply`. A truncated 65,536-byte prefix of a
    // larger document could already parse as `{"status":"repaired"}` and be
    // misread as `Repaired`. The fix streams the reply within
    // `MAX_REPAIR_REPLY_BYTES` so the cap bounds what is retained, not just
    // what is classified afterwards. Comments are stripped before matching,
    // so describing the read in a comment changes nothing.
    let source = production_source();
    let body = fn_body(source, "pub async fn call_repair", "#[cfg(test)]");
    let code = strip_comments(&body);
    assert!(
        !code.contains("bytes().await"),
        "call_repair must not collect the full reply with an unbounded \
         whole-body read before the cap (repair.rs:141); stream the reply \
         within MAX_REPAIR_REPLY_BYTES instead"
    );
    assert!(
        code.contains("MAX_REPAIR_REPLY_BYTES"),
        "call_repair must enforce MAX_REPAIR_REPLY_BYTES while reading the \
         reply, not only inside classify_repair_reply after the full body is buffered"
    );
}

#[test]
fn exact_ceiling_repaired_classifies_repaired() {
    let body = repaired_body(CEILING_BYTES);
    assert_eq!(body.len(), 65_536, "pin body must be exactly the ceiling");
    let value: serde_json::Value =
        serde_json::from_slice(&body).expect("pin body must stay well-formed JSON");
    assert_eq!(
        value.get("status").and_then(|s| s.as_str()),
        Some("repaired"),
        "pin body must keep status=repaired"
    );
    assert_eq!(
        classify_repair_reply(200, &body),
        RepairCall::Repaired,
        "a well-formed repaired reply at exactly 65,536 bytes must classify Repaired"
    );
}

#[test]
fn ceiling_plus_one_repaired_is_retry_from_length() {
    let body = repaired_body(CEILING_BYTES + 1);
    assert_eq!(body.len(), 65_537, "pin body must be exactly ceiling + 1");
    // Well-formed with status=repaired, so the Retry verdict below can only
    // come from the byte-length gate — never from parsing a truncated
    // document.
    let value: serde_json::Value =
        serde_json::from_slice(&body).expect("pin body must stay well-formed JSON");
    assert_eq!(
        value.get("status").and_then(|s| s.as_str()),
        Some("repaired"),
        "pin body must keep status=repaired so only the byte ceiling flips the verdict"
    );
    assert_eq!(
        classify_repair_reply(200, &body),
        RepairCall::Retry,
        "a well-formed repaired reply at 65,537 bytes must fail closed to Retry from its length"
    );
}
