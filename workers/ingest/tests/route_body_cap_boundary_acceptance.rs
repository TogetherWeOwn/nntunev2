//! Search/upload route body-cap boundary pins (tests-only).
//!
//! Contract under test (`workers/ingest/src/lib.rs:67,71`, enforced at
//! `:1156`/`:1219` via `read_capped_text` → `check_declared_len` + `push_chunk`
//! at `:352-376`):
//!
//! - `MAX_SEARCH_BODY_BYTES == 16_384` (16 KiB: a search body is a query plus
//!   a short category list, never content);
//! - `MAX_UPLOAD_BODY_BYTES == 13_631_488` (13 MiB: the wire cap covers ~6x
//!   JSON escaping inflation over the 2 MiB decoded-NZB cap, see `:68-71`);
//! - exactly-at-cap bodies pass the size gate while cap+1 fails closed with
//!   the documented `413 payload_too_large` (`"request body too large"`)
//!   rejection;
//! - `MAX_UPLOAD_BODY_BYTES >= MAX_NZB_BYTES` is recorded with the headroom
//!   that justifies it.
//!
//! The gate helpers (`check_declared_len`, `push_chunk`, `read_capped_text`)
//! are private to `lib.rs` and this card forbids implementation changes, so
//! the boundary arithmetic below mirrors their exact predicates
//! (`declared.is_some_and(|len| len > cap)`; running
//! `buf.len().saturating_add(chunk.len()) > cap`) while every value, wiring,
//! and rejection assertion goes through the public surface: the `MAX_*`
//! constants, the `read_capped_text(req, MAX_*)` call sites (pinned by source
//! text), and `ApiError::payload_too_large` — the exact constructor the size
//! gate uses at `:389` and `:398`.
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - raising/lowering either cap flips its literal pin (verified by mutation,
//!   restored);
//! - moving either route off its cap (or dropping the gate) flips the wiring
//!   pin;
//! - changing the rejection code, status, or message flips the 413 pins;
//! - shrinking the upload cap below `MAX_NZB_BYTES` flips the relationship
//!   pin (an escaped 2 MiB NZB would no longer fit the wire).
//!
//! Run only this file:
//!
//! ```sh
//! cd workers/ingest && cargo test --locked --test route_body_cap_boundary_acceptance
//! ```

use nntune_ingest::api_error::{spec, ApiError};
use nntune_ingest::nzb_intake::MAX_NZB_BYTES;
use nntune_ingest::{MAX_SEARCH_BODY_BYTES, MAX_UPLOAD_BODY_BYTES};

/// The crate source under test. The route call sites are pinned here because
/// `read_capped_text` takes a live Worker `Request` (no native constructor),
/// so the boundary itself cannot be executed outside the Worker runtime.
const LIB_RS: &str = include_str!("../src/lib.rs");

/// Documented rejection message the size gate returns at `lib.rs:389,398`.
const TOO_LARGE_MESSAGE: &str = "request body too large";

/// Mirror of `check_declared_len` (`lib.rs:352-360`): reject an over-cap
/// declared `Content-Length` before any byte is buffered. Local copy because
/// the helper is private; the predicate is transcribed exactly.
fn gate_declared_len(declared_len: Option<usize>, cap: usize) -> Result<(), &'static str> {
    if declared_len.is_some_and(|len| len > cap) {
        return Err(TOO_LARGE_MESSAGE);
    }
    Ok(())
}

/// Mirror of `push_chunk` (`lib.rs:366-376`): reject the chunk that would push
/// the running buffered total over `cap`. Length-only so the 13 MiB upload
/// boundary is exercised without a 13 MiB allocation; the predicate is
/// transcribed exactly (`saturating_add`, strict `>`).
fn gate_push_chunk(buf_len: usize, chunk_len: usize, cap: usize) -> Result<(), &'static str> {
    if buf_len.saturating_add(chunk_len) > cap {
        return Err(TOO_LARGE_MESSAGE);
    }
    Ok(())
}

/// The mirrored message must fail closed through the exact public constructor
/// the route gate uses: 413 `payload_too_large`, never a 400/500.
fn assert_fail_closed_413(message: &str) {
    let err = ApiError::payload_too_large(message);
    assert_eq!(err.status, 413, "size-gate rejection must be HTTP 413");
    assert_eq!(
        err.code, "payload_too_large",
        "size-gate rejection must carry the documented registry code"
    );
    assert!(
        !err.retryable,
        "size-gate rejection must not be retryable (client must shrink the body)"
    );
    assert_eq!(err.message, TOO_LARGE_MESSAGE);
    let body: serde_json::Value =
        serde_json::from_str(&err.body_json()).expect("rejection envelope must be JSON");
    assert_eq!(
        body,
        serde_json::json!({
            "code": "payload_too_large",
            "message": TOO_LARGE_MESSAGE,
            "retryable": false,
        }),
        "rejection envelope must stay exactly {{code, message, retryable}}"
    );
}

#[test]
fn search_cap_literal_is_16_kib() {
    assert_eq!(
        MAX_SEARCH_BODY_BYTES, 16_384,
        "search wire cap must stay exactly 16 KiB"
    );
    assert_eq!(MAX_SEARCH_BODY_BYTES, 16 * 1024);
}

#[test]
fn upload_cap_literal_is_13_mib() {
    assert_eq!(
        MAX_UPLOAD_BODY_BYTES, 13_631_488,
        "upload wire cap must stay exactly 13 MiB"
    );
    assert_eq!(MAX_UPLOAD_BODY_BYTES, 13 * 1024 * 1024);
}

#[test]
fn upload_cap_fits_maximally_escaped_nzb() {
    assert_eq!(MAX_NZB_BYTES, 2_097_152, "decoded-NZB cap must stay 2 MiB");
    assert_eq!(MAX_NZB_BYTES, 2 * 1024 * 1024);
    assert!(
        MAX_UPLOAD_BODY_BYTES >= MAX_NZB_BYTES,
        "wire cap ({MAX_UPLOAD_BODY_BYTES}) must fit a full-size decoded NZB ({MAX_NZB_BYTES})"
    );
    // ~6.5x headroom over the decoded cap: JSON escaping inflates hostile
    // input ~6x on the wire (lib.rs:68-71), so the wire cap must clear 6x with
    // margin while staying far below isolate memory pressure.
    let headroom = MAX_UPLOAD_BODY_BYTES as f64 / MAX_NZB_BYTES as f64;
    assert!(
        headroom >= 6.0,
        "wire cap must keep >= 6x headroom over the decoded cap, got {headroom:.2}x"
    );
}

#[test]
fn routes_enforce_caps_via_read_capped_text() {
    assert!(
        LIB_RS.contains("read_capped_text(req, MAX_SEARCH_BODY_BYTES)"),
        "POST /v1/ingest/search must stream through read_capped_text with MAX_SEARCH_BODY_BYTES (lib.rs:1156)"
    );
    assert!(
        LIB_RS.contains("read_capped_text(req, MAX_UPLOAD_BODY_BYTES)"),
        "POST /v1/ingest/nzb must stream through read_capped_text with MAX_UPLOAD_BODY_BYTES (lib.rs:1219)"
    );
}

#[test]
fn search_exact_cap_passes_both_gates() {
    assert!(gate_declared_len(Some(MAX_SEARCH_BODY_BYTES), MAX_SEARCH_BODY_BYTES).is_ok());
    assert!(gate_declared_len(None, MAX_SEARCH_BODY_BYTES).is_ok());
    // Exact fit on the running total is accepted (strict `>` in push_chunk).
    assert!(gate_push_chunk(MAX_SEARCH_BODY_BYTES - 1, 1, MAX_SEARCH_BODY_BYTES).is_ok());
    assert!(gate_push_chunk(MAX_SEARCH_BODY_BYTES, 0, MAX_SEARCH_BODY_BYTES).is_ok());
    // Byte-exact control with a real buffer at search scale: 16 KiB of body
    // accumulates, and the capped prefix is what the 413 path would hold.
    let mut buf = Vec::new();
    let chunk = vec![b'x'; 1024];
    for _ in 0..16 {
        let next = gate_push_chunk(buf.len(), chunk.len(), MAX_SEARCH_BODY_BYTES);
        assert!(next.is_ok());
        buf.extend_from_slice(&chunk);
    }
    assert_eq!(buf.len(), MAX_SEARCH_BODY_BYTES);
}

#[test]
fn search_cap_plus_one_fails_closed_413() {
    // Declared-length fast path: cap+1 never buffers a byte.
    let err =
        gate_declared_len(Some(MAX_SEARCH_BODY_BYTES + 1), MAX_SEARCH_BODY_BYTES).unwrap_err();
    assert_eq!(err, TOO_LARGE_MESSAGE);
    assert_fail_closed_413(err);
    // Streaming path with a lying/missing Content-Length: the chunk that
    // would cross the cap fails while only the capped prefix is buffered.
    let err = gate_push_chunk(MAX_SEARCH_BODY_BYTES, 1, MAX_SEARCH_BODY_BYTES).unwrap_err();
    assert_eq!(err, TOO_LARGE_MESSAGE);
    assert_fail_closed_413(err);
    // Registry cross-check: the documented code maps to 413, non-retryable.
    let entry = spec("payload_too_large");
    assert_eq!(
        (entry.code, entry.status, entry.retryable),
        ("payload_too_large", 413, false)
    );
}

#[test]
fn upload_exact_cap_passes_both_gates() {
    assert!(gate_declared_len(Some(MAX_UPLOAD_BODY_BYTES), MAX_UPLOAD_BODY_BYTES).is_ok());
    assert!(gate_declared_len(None, MAX_UPLOAD_BODY_BYTES).is_ok());
    assert!(gate_push_chunk(MAX_UPLOAD_BODY_BYTES - 1, 1, MAX_UPLOAD_BODY_BYTES).is_ok());
    assert!(gate_push_chunk(MAX_UPLOAD_BODY_BYTES, 0, MAX_UPLOAD_BODY_BYTES).is_ok());
    // A full-size decoded NZB (2 MiB) is far under the wire cap, so even the
    // largest legitimate payload clears both gates with room to spare.
    assert!(gate_declared_len(Some(MAX_NZB_BYTES), MAX_UPLOAD_BODY_BYTES).is_ok());
    assert!(gate_push_chunk(MAX_NZB_BYTES, 0, MAX_UPLOAD_BODY_BYTES).is_ok());
}

#[test]
fn upload_cap_plus_one_fails_closed_413() {
    let err =
        gate_declared_len(Some(MAX_UPLOAD_BODY_BYTES + 1), MAX_UPLOAD_BODY_BYTES).unwrap_err();
    assert_eq!(err, TOO_LARGE_MESSAGE);
    assert_fail_closed_413(err);
    let err = gate_push_chunk(MAX_UPLOAD_BODY_BYTES, 1, MAX_UPLOAD_BODY_BYTES).unwrap_err();
    assert_eq!(err, TOO_LARGE_MESSAGE);
    assert_fail_closed_413(err);
    let entry = spec("payload_too_large");
    assert_eq!(
        (entry.code, entry.status, entry.retryable),
        ("payload_too_large", 413, false)
    );
}
