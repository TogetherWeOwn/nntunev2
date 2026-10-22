//! NZB intake default count-cap pins (`workers/ingest/src/nzb_intake.rs`).
//!
//! Contract under test:
//!
//! - [`DEFAULT_LIMITS`] is the route policy: `max_files == 5_000`,
//!   `max_segments == 60_000`.
//! - A 5_001-file document under [`DEFAULT_LIMITS`] is rejected with
//!   [`IntakeRejection::TooManyFiles`] carrying `count == 5_001`.
//! - A 60_001-segment document under [`DEFAULT_LIMITS`] is rejected with
//!   [`IntakeRejection::TooManySegments`] carrying `count == 60_001`.
//! - At-cap documents (exactly 5_000 files / exactly 60_000 segments) pass.
//!
//! Both over-cap payloads stay well under [`MAX_NZB_BYTES`] so the count
//! cap (not the size cap) is what fires. In-file unit tests only exercise
//! custom limits; this integration file is the default-cap pin.

use nntune_ingest::nzb_intake::{
    estimate_nzb, estimate_nzb_with_limits, IntakeRejection, DEFAULT_LIMITS, MAX_FILES,
    MAX_NZB_BYTES, MAX_SEGMENTS,
};

fn file_bomb(count: usize) -> String {
    let mut bomb = String::from("<?xml?><nzb>");
    for _ in 0..count {
        bomb.push_str("<file></file>");
    }
    bomb.push_str("</nzb>");
    bomb
}

fn segment_bomb(count: usize) -> String {
    let mut bomb = String::from("<?xml?><nzb>");
    for _ in 0..count {
        bomb.push_str("<segment>x</segment>");
    }
    bomb.push_str("</nzb>");
    bomb
}

#[test]
fn default_limits_pin_expected_count_caps() {
    assert_eq!(DEFAULT_LIMITS.max_files, 5_000);
    assert_eq!(DEFAULT_LIMITS.max_files, MAX_FILES);
    assert_eq!(DEFAULT_LIMITS.max_segments, 60_000);
    assert_eq!(DEFAULT_LIMITS.max_segments, MAX_SEGMENTS);
}

#[test]
fn file_count_one_over_default_cap_is_rejected_with_exact_count() {
    let bomb = file_bomb(MAX_FILES + 1);
    assert!(bomb.len() < MAX_NZB_BYTES);
    let rejected = estimate_nzb(&bomb).unwrap_err();
    assert_eq!(rejected.code(), "NZB_TOO_MANY_FILES");
    assert_eq!(rejected.http_status(), 413);
    assert_eq!(
        rejected,
        IntakeRejection::TooManyFiles {
            count: 5_001,
            max: 5_000,
        }
    );
    // Same verdict through the explicit route policy object.
    assert_eq!(
        estimate_nzb_with_limits(&bomb, &DEFAULT_LIMITS).unwrap_err(),
        rejected
    );
}

#[test]
fn segment_count_one_over_default_cap_is_rejected_with_exact_count() {
    let bomb = segment_bomb(MAX_SEGMENTS + 1);
    assert!(bomb.len() < MAX_NZB_BYTES);
    let rejected = estimate_nzb(&bomb).unwrap_err();
    assert_eq!(rejected.code(), "NZB_TOO_MANY_SEGMENTS");
    assert_eq!(rejected.http_status(), 413);
    assert_eq!(
        rejected,
        IntakeRejection::TooManySegments {
            count: 60_001,
            max: 60_000,
        }
    );
    assert_eq!(
        estimate_nzb_with_limits(&bomb, &DEFAULT_LIMITS).unwrap_err(),
        rejected
    );
}

#[test]
fn at_cap_file_count_passes_under_default_limits() {
    let doc = file_bomb(MAX_FILES);
    assert!(doc.len() < MAX_NZB_BYTES);
    let est = estimate_nzb(&doc).unwrap();
    assert_eq!(est.file_count, 5_000);
    assert_eq!(est.segment_count, 0);
    assert_eq!(
        estimate_nzb_with_limits(&doc, &DEFAULT_LIMITS)
            .unwrap()
            .file_count,
        5_000
    );
}

#[test]
fn at_cap_segment_count_passes_under_default_limits() {
    let doc = segment_bomb(MAX_SEGMENTS);
    assert!(doc.len() < MAX_NZB_BYTES);
    let est = estimate_nzb(&doc).unwrap();
    assert_eq!(est.file_count, 0);
    assert_eq!(est.segment_count, 60_000);
    assert_eq!(
        estimate_nzb_with_limits(&doc, &DEFAULT_LIMITS)
            .unwrap()
            .segment_count,
        60_000
    );
}
