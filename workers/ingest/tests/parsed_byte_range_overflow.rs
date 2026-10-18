//! `check_parsed` segment-byte overflow and 64 GiB document-cap pins.
//!
//! Contract under test (`workers/ingest/src/parse.rs:177-189`):
//!
//! - `segment_bytes` folds with `checked_add`: a bad segment (`Some(0)` or
//!   above `MAX_SEGMENT_BYTES`) or a `u64`-overflowing sum yields exactly
//!   `"segment byte ranges exceed their valid bounds"`;
//! - a representable total above 64 GiB (`MAX_ENCODED_BYTES`) yields exactly
//!   `"total segment byte ranges exceed the document limit"`.
//!
//! - hostile magnitudes (`u64::MAX`, whose two-wide true sum `2^65 - 2`
//!   overflows `u64`) yield the bounds error via the per-segment guard —
//!   never a wrap or panic. No sum of in-bounds segments can overflow `u64`
//!   (it would take 8,589,934,597 maxed segments), so the guard is the
//!   load-bearing arm for hostile magnitudes and this is the closest
//!   observable `u64`-overflow proxy through `check_parsed`;
//! - 33 maxed segments (total `33 * MAX_SEGMENT_BYTES = 70,866,960,351`,
//!   ~2 GiB over the cap) yield the document-limit error — a
//!   `checked_add` -> `wrapping_add` mutant still reports this (the sum
//!   fits `u64`), so this case pins the string, not the fold;
//! - 32 maxed segments (total `64 GiB - 32`, representable and in-bounds)
//!   pass: the just-under-cap control. Removing the document-limit check
//!   or the per-segment guard turns a pinned `Err` into `Ok`, so the
//!   error cases fail under those mutants.
//!
//! Fixture-built `ParsedNzb` values with valid message-ids throughout, so
//! the only possible failure is the byte-range fold. Implementation stays
//! untouched.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked --manifest-path workers/ingest/Cargo.toml --test parsed_byte_range_overflow
//! ```

use nntune_ingest::nzb_intake::IntakeEstimate;
use nntune_ingest::parse::{check_parsed, ParsedNzb};
use nntune_ingest::probe::MAX_SEGMENT_BYTES;

/// 64 GiB document cap (`MAX_ENCODED_BYTES` in `parse.rs`, private there).
const DOCUMENT_CAP: u64 = 64 << 30;

const BOUNDS_MSG: &str = "segment byte ranges exceed their valid bounds";
const DOCUMENT_LIMIT_MSG: &str = "total segment byte ranges exceed the document limit";

/// One file, `bytes.len()` segments, valid ids, flat and per-file ranges in
/// agreement: the only thing `check_parsed` may reject is the byte fold.
fn parsed_with_bytes(bytes: Vec<Option<u64>>) -> (IntakeEstimate, ParsedNzb) {
    let segment_count = bytes.len();
    let message_ids: Vec<String> = (0..segment_count)
        .map(|i| format!("seg-{i}@synthetic.invalid"))
        .collect();
    let estimate = IntakeEstimate {
        file_count: 1,
        segment_count,
        declared_bytes: 0,
    };
    let parsed = ParsedNzb {
        file_count: 1,
        segment_count,
        message_ids,
        segment_bytes: bytes.clone(),
        file_segment_bytes: vec![bytes],
    };
    (estimate, parsed)
}

#[test]
fn zero_segment_byte_yields_bounds_error() {
    let (estimate, parsed) = parsed_with_bytes(vec![Some(100), Some(0)]);
    let err = check_parsed(&estimate, &parsed).expect_err("Some(0) is never a valid range");
    assert_eq!(err, BOUNDS_MSG);
}

#[test]
fn over_cap_segment_byte_yields_bounds_error() {
    let over_cap = MAX_SEGMENT_BYTES + 1;
    let (estimate, parsed) = parsed_with_bytes(vec![Some(100), Some(over_cap)]);
    let err =
        check_parsed(&estimate, &parsed).expect_err("above-cap segment bytes must fail the fold");
    assert_eq!(err, BOUNDS_MSG);
}

#[test]
fn hostile_u64_max_magnitudes_yield_bounds_error_without_wrapping() {
    // True sum 2^65 - 2 overflows u64: must fail closed with the bounds
    // error, never wrap to a small total or panic.
    assert_eq!(u64::MAX.checked_add(u64::MAX), None);
    let (estimate, parsed) = parsed_with_bytes(vec![Some(u64::MAX), Some(u64::MAX)]);
    let err = check_parsed(&estimate, &parsed).expect_err("u64::MAX magnitudes must fail the fold");
    assert_eq!(err, BOUNDS_MSG);
}

#[test]
fn total_over_64gib_yields_document_limit_error() {
    // 33 maxed segments: representable, every segment in-bounds, total
    // ~2 GiB over the cap.
    assert_eq!(33u64 * MAX_SEGMENT_BYTES, 70_866_960_351);
    const { assert!(33u64 * MAX_SEGMENT_BYTES > DOCUMENT_CAP) };
    let (estimate, parsed) = parsed_with_bytes(vec![Some(MAX_SEGMENT_BYTES); 33]);
    let err =
        check_parsed(&estimate, &parsed).expect_err("total over 64 GiB must fail the document cap");
    assert_eq!(err, DOCUMENT_LIMIT_MSG);
}

#[test]
fn just_under_cap_control_passes() {
    // 32 maxed segments: every segment in-bounds, total exactly 32 bytes
    // under the cap.
    assert_eq!(32u64 * MAX_SEGMENT_BYTES, DOCUMENT_CAP - 32);
    let (estimate, parsed) = parsed_with_bytes(vec![Some(MAX_SEGMENT_BYTES); 32]);
    check_parsed(&estimate, &parsed).expect("just-under-cap total must pass");
}
