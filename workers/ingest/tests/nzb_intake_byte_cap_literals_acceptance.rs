//! NZB intake byte-cap literal pins (`workers/ingest/src/nzb_intake.rs:42`).
//!
//! Contract under test:
//!
//! - `MAX_NZB_BYTES == 2_097_152` (`2 * 1024 * 1024`): the largest accepted
//!   upload, enforced as `body.len() > max_nzb_bytes` fail-closed to
//!   [`IntakeRejection::TooLarge`] before any XML work in
//!   `estimate_nzb_with_limits`.
//! - [`DEFAULT_LIMITS`] is the route policy: `max_nzb_bytes == MAX_NZB_BYTES`.
//! - An exactly-at-cap well-formed NZB document passes the size gate through
//!   the public intake helper (`estimate_nzb`); the same shape at cap+1 fails
//!   closed with `TooLarge { size_bytes: cap + 1, max_bytes: cap }`
//!   (`NZB_TOO_LARGE`, 413).
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - raising the cap (e.g. 4 MiB) lets the cap+1 document through, failing
//!   `cap_plus_one_fails_closed_through_public_helper`;
//! - lowering the cap rejects the exactly-at-cap document, failing
//!   `exactly_at_cap_passes_size_gate_through_public_helper`;
//! - moving the length check after the XML parse (or dropping it) lets the
//!   cap+1 document reach `NotNzb`/`Malformed` instead of `TooLarge`, failing
//!   the cap+1 pin — the in-module oversize test at `nzb_intake.rs` uses a
//!   non-NZB filler, so only this file pins the boundary with a well-formed
//!   NZB on both sides.
//!
//! Run only this file:
//!
//! ```sh
//! cd workers/ingest && cargo test --locked --test nzb_intake_byte_cap_literals_acceptance
//! ```

use nntune_ingest::nzb_intake::{
    estimate_nzb, estimate_nzb_with_limits, IntakeRejection, DEFAULT_LIMITS, MAX_NZB_BYTES,
};

/// Byte cap under test: `MAX_NZB_BYTES = 2 * 1024 * 1024`.
const CAP_BYTES: usize = 2_097_152;

const PREFIX: &str = "<?xml version=\"1.0\"?><nzb>";
const SUFFIX: &str = "</nzb>";

/// Build a well-formed `<nzb>` document of exactly `total_len` bytes. Padding
/// is plain `a` text inside the root element, so the document stays
/// well-formed at any size and the only variable the intake helper may react
/// to at the boundary is the byte length.
fn sized_nzb(total_len: usize) -> String {
    assert!(
        total_len >= PREFIX.len() + SUFFIX.len(),
        "document must fit the nzb envelope"
    );
    let pad_len = total_len - PREFIX.len() - SUFFIX.len();
    let mut doc = String::with_capacity(total_len);
    doc.push_str(PREFIX);
    doc.push_str(&"a".repeat(pad_len));
    doc.push_str(SUFFIX);
    debug_assert_eq!(doc.len(), total_len);
    doc
}

#[test]
fn max_nzb_bytes_is_two_mebibytes() {
    assert_eq!(
        MAX_NZB_BYTES, 2_097_152,
        "NZB intake byte cap must stay exactly 2 MiB (2 * 1024 * 1024)"
    );
    assert_eq!(
        MAX_NZB_BYTES, CAP_BYTES,
        "pin constant must track the implementation cap"
    );
    assert_eq!(
        DEFAULT_LIMITS.max_nzb_bytes, MAX_NZB_BYTES,
        "route policy must enforce the bare cap, not a copy"
    );
    assert_eq!(
        DEFAULT_LIMITS.max_nzb_bytes, 2_097_152,
        "route policy byte cap must stay exactly 2_097_152"
    );
}

#[test]
fn exactly_at_cap_passes_size_gate_through_public_helper() {
    let doc = sized_nzb(CAP_BYTES);
    assert_eq!(doc.len(), 2_097_152, "pin body must be exactly the cap");
    let est = estimate_nzb(&doc).expect("exactly-at-cap NZB must pass the size gate");
    assert_eq!(est.file_count, 0);
    assert_eq!(est.segment_count, 0);
    assert_eq!(est.declared_bytes, 0);
    // Same verdict through the explicit route policy object.
    assert!(
        estimate_nzb_with_limits(&doc, &DEFAULT_LIMITS).is_ok(),
        "exactly-at-cap NZB must pass under DEFAULT_LIMITS"
    );
}

#[test]
fn cap_plus_one_fails_closed_through_public_helper() {
    let doc = sized_nzb(CAP_BYTES + 1);
    assert_eq!(doc.len(), 2_097_153, "pin body must be exactly cap + 1");
    let rejected = estimate_nzb(&doc).unwrap_err();
    assert_eq!(rejected.code(), "NZB_TOO_LARGE");
    assert_eq!(rejected.http_status(), 413);
    assert_eq!(
        rejected,
        IntakeRejection::TooLarge {
            size_bytes: 2_097_152 + 1,
            max_bytes: 2_097_152,
        },
        "cap+1 must fail closed carrying the measured size and the cap"
    );
    assert_eq!(
        estimate_nzb_with_limits(&doc, &DEFAULT_LIMITS).unwrap_err(),
        rejected,
        "same verdict through the explicit route policy object"
    );
}
