//! Pool-body reply retention guard (tests-only).
//!
//! Contract under test (`workers/ingest/src/pool_body.rs`):
//!
//! - `call_pool_body` must not retain the whole pool body reply before
//!   enforcing `MAX_BODY_REPLY_BYTES` (16 MiB). Buffering the full reply
//!   first (today: `response.bytes().await` in `call_pool_body`) holds an
//!   unbounded body in memory and only afterwards lets `classify_body_reply`
//!   reject a `200` longer than the cap.
//! - A silently truncated 16 MiB prefix passed to `classify_body_reply`
//!   classifies as `BodyFetch::Bytes`, not `Rejected`, so truncation is not
//!   a fix: the fetch path itself must observe that more bytes existed and
//!   fail closed as `PoolBodyError::Rejected`.
//! - Boundary behavior through the public classifier entry stays intact: an
//!   exactly-at-cap `200` is `BodyFetch::Bytes`; cap+1 is `Rejected`.
//!
//! The fetch entry needs a live Worker `Env` plus a pool `Response`, so the
//! retention pins read the production source via `include_str!` (sliced to
//! the pre-`#[cfg(test)]` half so unit-test bodies cannot satisfy them, and
//! comment-stripped so a comment mentioning the buffered call cannot satisfy
//! them), while the boundary pins drive the public classifier entry.
//!
//! Sensitivity (implementation stays untouched by this file):
//!
//! - buffering the full reply in `call_pool_body` (any `.bytes()` / `.text()`
//!   drain before the cap) fails `fetcher_must_not_buffer_unbounded_body`;
//! - enforcing the cap only inside `classify_body_reply` (no
//!   `MAX_BODY_REPLY_BYTES` / `Rejected` in `call_pool_body`) fails
//!   `fetcher_must_enforce_cap_and_fail_closed_as_rejected`;
//! - truncating to the cap and delegating the prefix to `classify_body_reply`
//!   keeps the classifier pin green as `Bytes` but leaves no `Rejected` in
//!   the fetcher, failing the same enforcement pin — the fetcher must know
//!   more bytes existed;
//! - raising/lowering the cap or flipping the classifier comparison breaks
//!   one of the two boundary controls below (the literal itself stays pinned
//!   by the sibling size-cap suite, untouched here).
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked -p nntune-ingest --test pool_body_reply_retention
//! ```

use nntune_ingest::pool_body::{
    classify_body_reply, BodyFetch, PoolBodyError, MAX_BODY_REPLY_BYTES,
};

/// Production source before the unit-test module, so helper bodies in
/// `#[cfg(test)]` cannot satisfy the wiring pins.
fn production_source() -> &'static str {
    let source = include_str!("../src/pool_body.rs");
    source
        .split_once("#[cfg(test)]")
        .map(|(head, _)| head)
        .unwrap_or(source)
}

/// Strip `//` line comments and `/* … */` block comments, respecting
/// double-quoted strings and `'x'` char literals so a `//` inside a URL
/// such as `"https://…"` is not treated as a comment start. The retention
/// pins run on this stripped code so a comment mentioning the buffered
/// call cannot satisfy them.
fn code_without_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    let mut in_string = false;
    let mut in_char = false;
    let mut escaped = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            out.push(b as char);
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if in_char {
            out.push(b as char);
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'\'' {
                in_char = false;
            }
            i += 1;
            continue;
        }
        // Not in a string/char: look for comment or string starts.
        if b == b'"' {
            in_string = true;
            out.push(b as char);
            i += 1;
            continue;
        }
        if b == b'\'' {
            in_char = true;
            out.push(b as char);
            i += 1;
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            // Line comment: skip to (but keep) the newline.
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            // Block comment: skip to the closing `*/`.
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            // Keep a single space so tokens on either side stay separated.
            out.push(' ');
            continue;
        }
        out.push(b as char);
        i += 1;
    }
    out
}

/// Code of the fetch entry (`call_pool_body`) sliced from the first
/// `fn call_pool_body` declaration to the end of the pre-test source, then
/// comment-stripped. Slicing keeps the classifier's own legitimate
/// `MAX_BODY_REPLY_BYTES` / `Rejected` handling from satisfying the fetcher
/// pins, and the strip keeps comments from satisfying them either.
fn call_pool_body_code() -> String {
    let production = production_source();
    let start = production
        .find("fn call_pool_body")
        .expect("pool_body.rs must define fn call_pool_body");
    code_without_comments(&production[start..])
}

#[test]
fn exact_cap_still_classifies_as_bytes() {
    let at_cap = vec![0x41u8; MAX_BODY_REPLY_BYTES];
    assert_eq!(
        at_cap.len(),
        MAX_BODY_REPLY_BYTES,
        "control body must be exactly the cap"
    );
    match classify_body_reply(200, None, &at_cap) {
        Ok(BodyFetch::Bytes(bytes)) => {
            assert_eq!(
                bytes.len(),
                MAX_BODY_REPLY_BYTES,
                "exactly-at-cap reply must round-trip every byte"
            );
            assert_eq!(bytes, at_cap, "exactly-at-cap reply bytes must be intact");
        }
        other => panic!("exactly-at-cap 200 must pass as Bytes, got {other:?}"),
    }
}

#[test]
fn cap_plus_one_still_rejects() {
    let oversize = vec![0x41u8; MAX_BODY_REPLY_BYTES + 1];
    assert_eq!(
        oversize.len(),
        MAX_BODY_REPLY_BYTES + 1,
        "control body must be exactly cap + 1"
    );
    assert_eq!(
        classify_body_reply(200, None, &oversize),
        Err(PoolBodyError::Rejected),
        "cap+1 200 reply must fail closed as Rejected, never buffered as Bytes"
    );
}

#[test]
fn fetcher_must_not_buffer_unbounded_body_before_cap() {
    let fetcher = call_pool_body_code();
    assert!(
        !fetcher.contains(".bytes()"),
        "call_pool_body must not drain the pool reply with response.bytes() before the cap: \
         that retains the whole (unbounded) body first and only then rejects oversize"
    );
    assert!(
        !fetcher.contains(".text()"),
        "call_pool_body must not drain the pool reply with response.text() before the cap: \
         that retains the whole body as text first instead of enforcing the byte cap while reading"
    );
    assert!(
        !fetcher.contains(".json()"),
        "call_pool_body must not drain the pool reply with response.json() before the cap"
    );
}

#[test]
fn fetcher_must_enforce_cap_and_fail_closed_as_rejected() {
    let fetcher = call_pool_body_code();
    assert!(
        fetcher.contains("MAX_BODY_REPLY_BYTES"),
        "call_pool_body itself must enforce MAX_BODY_REPLY_BYTES while reading: \
         leaving the cap to classify_body_reply means the full body was already retained"
    );
    assert!(
        fetcher.contains("Rejected"),
        "call_pool_body must be able to fail closed as PoolBodyError::Rejected on oversize: \
         passing a silently truncated 16 MiB prefix to classify_body_reply yields Bytes, not Rejected, \
         so the fetcher must observe that more bytes existed"
    );
}
