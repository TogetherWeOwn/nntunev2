//! Pool-probe reply size-cap acceptance (tests-only).
//!
//! Contract under test (`workers/ingest/src/pool_probe.rs:112`):
//!
//! - New ingest `MAX_PROBE_REPLY_BYTES == 1024 * 1024` (1 MiB), separate from
//!   the stream request cap `MAX_PROBE_BODY_BYTES`
//!   (`workers/stream/src/probe.rs:50`). A normal outcome is `batch_index`
//!   plus `present` and `missing`; a small one must keep folding through
//!   `check_outcome`.
//! - `call_pool_probe` bounds the reply read with that cap before
//!   classification: no unbounded `response.text().await` retention. The
//!   byte/chunk that would cross the cap fails closed as
//!   `PoolProbeError::Rejected` before any JSON parse.
//! - An oversize body is `Rejected` because more bytes existed — never parsed
//!   from a truncated prefix. A cap-length prefix that alone would classify
//!   `Ok` must still reject when the full body is over cap.
//! - Classifier controls: a small JSON outcome still folds through
//!   `check_outcome`; a `200` that is not JSON is `Rejected`.
//! - `MAX_PROBE_SEGMENTS` stays 500; `workers/stream/src/probe.rs` is untouched.
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - removing the `MAX_PROBE_REPLY_BYTES` declaration (or changing its value
//!   or expression) fails `reply_cap_constant_is_1_mib_separate_from_stream_cap`;
//!   a comment that merely mentions the cap does not count (comments are
//!   stripped before every source pin);
//! - restoring the unbounded `response.text().await` read, dropping the
//!   `> MAX_PROBE_REPLY_BYTES` comparison, mapping oversize to anything but
//!   `Rejected`, or moving the cap after the `classify_reply` call fails
//!   `call_pool_probe_bounds_the_read_before_classification`;
//! - parsing a truncated prefix (slice/`truncate` the oversize body then
//!   classify the prefix) fails `oversize_must_not_parse_from_truncated_prefix`
//!   (the prefix alone is `Ok`, the full body must be `Rejected`) and the
//!   no-truncation pins;
//! - weakening the comparison to `>=` rejects the exactly-at-cap body, failing
//!   `exact_cap_wellformed_is_admitted`;
//! - breaking the small-outcome or non-JSON mappings fails the control pins.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked -p nntune-ingest --test pool_probe_reply_cap
//! ```

use nntune_ingest::pool_probe::{check_outcome, classify_reply, PoolProbeError};
use nntune_ingest::probe::{ProbeOutcome, ProbeRequest, MAX_PROBE_SEGMENTS};

/// Expected reply cap: `MAX_PROBE_REPLY_BYTES = 1024 * 1024` (1 MiB).
const EXPECTED_CAP_BYTES: usize = 1024 * 1024;

/// The crate source under test. Reply-cap wiring is pinned here because
/// `call_pool_probe` takes a live Worker `Env` (no native constructor), so
/// the bounded read itself cannot be executed outside the Worker runtime —
/// like `route_body_cap_boundary_acceptance`, values and call-site wiring are
/// pinned by source text while every verdict assertion goes through the
/// public classifier (`classify_reply` / `check_outcome`).
const POOL_PROBE_RS: &str = include_str!("../src/pool_probe.rs");

/// Production source before the unit-test module: pins below must match the
/// shipped code, never this file's own literals.
fn production_source() -> &'static str {
    POOL_PROBE_RS
        .split_once("#[cfg(test)]")
        .expect("test module must exist")
        .0
}

/// Strip `//` line comments and `/* */` block comments (string-aware, so
/// `"https://…"` URLs inside literals survive). A comment that mentions the
/// cap therefore cannot satisfy any source pin below.
fn strip_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let bytes = source.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    let mut in_block = false;
    while i < bytes.len() {
        if in_block {
            if bytes[i] == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                in_block = false;
                i += 2;
            } else {
                // Keep newlines so line numbers survive; drop comment bytes.
                if bytes[i] == b'\n' {
                    out.push('\n');
                }
                i += 1;
            }
            continue;
        }
        if in_string {
            out.push(bytes[i] as char);
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if bytes[i] == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if bytes[i] == b'"' {
            in_string = true;
            out.push('"');
            i += 1;
            continue;
        }
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            // Skip to end of line (keep the newline).
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            in_block = true;
            i += 2;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Code-only production source: comments stripped, so prose about the cap
/// never counts as the cap.
fn code_source() -> String {
    strip_comments(production_source())
}

/// Body of `call_pool_probe` out of the code-only production source, bounded
/// by the test module, so wiring pins cannot match a neighboring function.
fn call_probe_body() -> String {
    let code = code_source();
    let after = code
        .split_once("async fn call_pool_probe(")
        .expect("call_pool_probe must exist")
        .1;
    after
        .split_once("#[cfg(test)]")
        .map(|(body, _)| body)
        .unwrap_or(after)
        .to_string()
}

fn request() -> ProbeRequest {
    ProbeRequest {
        job_id: "job-1".to_owned(),
        user_id: "user-1".to_owned(),
        idempotency_key: "key-1".to_owned(),
        batch_index: 3,
        segment_ids: vec![
            "segment-a@synthetic.invalid".to_owned(),
            "segment-b@synthetic.invalid".to_owned(),
        ],
        segment_bytes: vec![None, None],
    }
}

fn valid_body(req: &ProbeRequest) -> String {
    serde_json::json!({
        "batch_index": req.batch_index,
        "present": [req.segment_ids[0]],
        "missing": [req.segment_ids[1]],
    })
    .to_string()
}

/// Well-formed outcome body of exactly `total_len` bytes. Padding lives inside
/// the ignored `pad` JSON string value (plain `x` bytes, no escaping), so the
/// body stays well-formed at any size and the only variable the classifier may
/// react to is the byte length. Unknown `pad` is ignored by the outcome shape,
/// so the partitioned ids still fold through `check_outcome`.
fn padded_body(req: &ProbeRequest, total_len: usize) -> String {
    let prefix = format!(
        "{{\"batch_index\":{},\"present\":[\"{}\"],\"missing\":[\"{}\"],\"pad\":\"",
        req.batch_index, req.segment_ids[0], req.segment_ids[1]
    );
    const SUFFIX: &str = "\"}";
    assert!(
        total_len >= prefix.len() + SUFFIX.len(),
        "body must fit the outcome envelope"
    );
    let pad_len = total_len - prefix.len() - SUFFIX.len();
    let mut body = String::with_capacity(total_len);
    body.push_str(&prefix);
    body.push_str(&"x".repeat(pad_len));
    body.push_str(SUFFIX);
    assert_eq!(body.len(), total_len, "pin body must be byte-exact");
    body
}

/// Valid small outcome plus trailing spaces to exactly `total_len` bytes.
/// Trailing whitespace keeps the JSON well-formed, so the cap-length prefix of
/// an oversize body of this shape is itself a valid outcome: a truncating
/// implementation would parse the prefix as `Ok`, while the correct verdict
/// for the full oversize body is `Rejected` because more bytes existed.
fn trailing_spaces_body(req: &ProbeRequest, total_len: usize) -> String {
    let base = valid_body(req);
    assert!(
        base.len() < total_len,
        "small outcome must leave room for the oversize tail"
    );
    let mut body = base;
    body.push_str(&" ".repeat(total_len - body.len()));
    assert_eq!(body.len(), total_len, "pin body must be byte-exact");
    body
}

#[test]
fn reply_cap_constant_is_1_mib_separate_from_stream_cap() {
    assert_eq!(
        EXPECTED_CAP_BYTES, 1_048_576,
        "reply cap must stay exactly 1 MiB"
    );
    assert_eq!(EXPECTED_CAP_BYTES, 1024 * 1024);
    let code = code_source();
    let declared = code
        .lines()
        .any(|line| line.contains("const MAX_PROBE_REPLY_BYTES") && line.contains("1024 * 1024"));
    assert!(
        declared,
        "pool_probe.rs must declare `MAX_PROBE_REPLY_BYTES` as exactly `1024 * 1024` \
         on a code line (comments do not count)"
    );
    assert!(
        !code.contains("MAX_PROBE_BODY_BYTES"),
        "the ingest reply cap must be its own constant, not the stream request cap"
    );
}

#[test]
fn probe_batch_cap_stays_500() {
    assert_eq!(MAX_PROBE_SEGMENTS, 500, "the probe batch cap must stay 500");
    assert!(
        !code_source().contains("const MAX_PROBE_SEGMENTS"),
        "pool_probe.rs must not redefine the batch cap"
    );
}

#[test]
fn small_outcome_still_folds_through_check_outcome() {
    let req = request();
    let body = valid_body(&req);
    let outcome = classify_reply(&req, 200, None, &body).expect("valid pool reply");
    assert_eq!(
        outcome,
        ProbeOutcome {
            batch_index: req.batch_index,
            present: vec![req.segment_ids[0].clone()],
            missing: vec![req.segment_ids[1].clone()],
        }
    );
    assert_eq!(check_outcome(&req, &outcome), Ok(()));
}

#[test]
fn non_json_200_is_rejected() {
    let req = request();
    assert_eq!(
        classify_reply(&req, 200, None, "not json"),
        Err(PoolProbeError::Rejected),
        "a 200 that fails JSON parse is already Rejected"
    );
}

#[test]
fn exact_cap_wellformed_is_admitted() {
    let req = request();
    let body = padded_body(&req, EXPECTED_CAP_BYTES);
    assert_eq!(body.len(), 1_048_576, "pin body must be exactly the cap");
    let value: serde_json::Value =
        serde_json::from_str(&body).expect("pin body must stay well-formed JSON");
    assert_eq!(
        value.get("batch_index").and_then(|v| v.as_u64()),
        Some(req.batch_index as u64),
        "pin body must keep the request batch index"
    );
    let outcome =
        classify_reply(&req, 200, None, &body).expect("exactly-at-cap reply must classify");
    assert_eq!(check_outcome(&req, &outcome), Ok(()));
}

#[test]
fn oversize_wellformed_is_rejected_because_more_bytes_existed() {
    let req = request();
    let body = padded_body(&req, EXPECTED_CAP_BYTES + 1);
    assert_eq!(
        body.len(),
        1_048_576 + 1,
        "pin body must be exactly cap + 1"
    );
    let value: serde_json::Value =
        serde_json::from_str(&body).expect("pin body must stay well-formed JSON");
    assert_eq!(
        value.get("batch_index").and_then(|v| v.as_u64()),
        Some(req.batch_index as u64),
        "pin body must keep the request batch index so only the byte cap flips the verdict"
    );
    assert_eq!(
        classify_reply(&req, 200, None, &body),
        Err(PoolProbeError::Rejected),
        "a well-formed outcome at cap + 1 must fail closed as Rejected, \
         never parsed: more bytes existed than the cap allows"
    );
}

#[test]
fn oversize_must_not_parse_from_truncated_prefix() {
    let req = request();
    let oversize = trailing_spaces_body(&req, EXPECTED_CAP_BYTES + 1);
    assert_eq!(
        oversize.len(),
        1_048_576 + 1,
        "pin body must be exactly cap + 1"
    );
    // The cap-length prefix alone is a valid outcome: a truncating
    // implementation would read this prefix and classify `Ok`.
    let prefix = &oversize[..EXPECTED_CAP_BYTES];
    assert_eq!(prefix.len(), 1_048_576);
    let prefix_outcome =
        classify_reply(&req, 200, None, prefix).expect("cap-length prefix parses as Ok");
    assert_eq!(check_outcome(&req, &prefix_outcome), Ok(()));
    // The full oversize body must still reject: the verdict comes from the
    // extra byte existing, not from parsing the prefix.
    assert_eq!(
        classify_reply(&req, 200, None, &oversize),
        Err(PoolProbeError::Rejected),
        "an oversize body whose truncated prefix would parse must still be \
         Rejected because more bytes existed"
    );
}

#[test]
fn call_pool_probe_bounds_the_read_before_classification() {
    let body = call_probe_body();
    assert!(
        body.contains("MAX_PROBE_REPLY_BYTES"),
        "call_pool_probe must enforce MAX_PROBE_REPLY_BYTES (pool_probe.rs:112 \
         reads unbounded response.text().await today)"
    );
    assert!(
        code_source().contains("> MAX_PROBE_REPLY_BYTES"),
        "the reply path must reject strictly over the cap (`>`, so the exact fit is admitted)"
    );
    assert!(
        body.contains("Rejected"),
        "an over-cap pool reply must fail closed as Rejected"
    );
    assert!(
        !body.contains("response.text().await") && !body.contains(".text().await"),
        "call_pool_probe must not retain the reply through unbounded response.text().await"
    );
    assert!(
        !body.contains(".truncate("),
        "an over-cap reply must be rejected whole, never truncated then parsed"
    );
    assert!(
        !body
            .lines()
            .any(|line| line.contains("[..") && line.contains("MAX_PROBE_REPLY_BYTES")),
        "an over-cap reply must be rejected whole, never sliced to the cap then parsed"
    );
    let cap = body
        .find("MAX_PROBE_REPLY_BYTES")
        .expect("cap use must exist");
    let classify = body
        .find("classify_reply")
        .expect("call_pool_probe must still classify the reply");
    assert!(
        cap < classify,
        "the size cap must apply before classification, not after parsing"
    );
    // Pin the in-loop streaming ceiling: the chunk that would cross the cap
    // fails closed before it is retained. Deleting the three-line
    // `saturating_add` guard leaves only the buffered-fallback comparison, so
    // the loop guard itself must be pinned here, not just any cap comparison.
    assert!(
        body.lines().any(|line| line.contains("saturating_add")
            && (line.contains("reply_cap") || line.contains("MAX_PROBE_REPLY_BYTES"))),
        "the streaming loop must compare buf.len().saturating_add(chunk.len()) \
         against the reply cap before retaining the chunk"
    );
    let saturating = body
        .find("saturating_add")
        .expect("streaming loop must accumulate with saturating_add");
    let extend = body
        .find("extend_from_slice")
        .expect("streaming loop must retain chunks with extend_from_slice");
    assert!(
        saturating < extend,
        "the in-loop saturating_add cap check must precede extend_from_slice, \
         so the over-cap chunk is rejected before it is retained"
    );
    // Pin the non-200 early return: a non-200 verdict never reads the body.
    // Deleting the `status != 200` early return would send every status through
    // the streaming read, so the branch, its position before the stream, and
    // the `from_utf8` decode after the stream must all be pinned.
    let status_branch = body
        .find("status != 200")
        .or_else(|| body.find("status!=200"))
        .expect("non-200 replies must branch on status != 200 before any read");
    let stream = body
        .find(".stream()")
        .expect("the bounded 200 read must stream the body");
    let decode = body
        .find("from_utf8")
        .expect("the streamed bytes must decode with from_utf8 after the stream");
    assert!(
        status_branch < stream,
        "the status != 200 return must precede the streaming read, so non-200 \
         verdicts never read the body"
    );
    assert!(
        stream < decode,
        "from_utf8 must follow the stream, so only the bounded 200 bytes decode"
    );
    assert!(
        body[status_branch..stream].contains("return")
            && body[status_branch..stream].contains("classify_reply")
            && body[status_branch..stream].contains("\"\""),
        "the non-200 branch must return classify_reply with an empty body, \
         never the streamed bytes"
    );
    // Pin the over-cap Rejected passthrough: the raced 200 body read fails
    // closed as Rejected when more bytes existed; transport failure and expiry
    // share the expired Retry. Deleting this arm sends an over-cap 200 through
    // the wildcard expiry arm as expired Retry (redelivered + re-probed)
    // instead of permanent Rejected.
    let body_race = body
        .find("race_against_deadline(body_read")
        .expect("the bounded 200 read must race body_read against the shared deadline");
    let after_race = &body[body_race..];
    let passthrough_rel = after_race
        .find("Some(Err(PoolProbeError::Rejected)) => return Err(PoolProbeError::Rejected)")
        .expect(
            "the body-stage match must pass Rejected through as Rejected \
             (over-cap 200 fails closed, never expired Retry)",
        );
    let expiry_rel = after_race
        .find("Some(Err(_)) | None => return Err(expired)")
        .expect("the body-stage match must share transport failure and expiry as expired Retry");
    assert!(
        passthrough_rel < expiry_rel,
        "the Rejected passthrough must precede the body-stage expiry arm, \
         so an over-cap 200 returns Rejected instead of expired Retry"
    );
}
