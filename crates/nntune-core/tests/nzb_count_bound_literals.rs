//! Pin for the NZB file-count and segments-per-file bounds
//! (`crates/nntune-core/src/nzb.rs`): `MAX_FILES` (1024) and
//! `MAX_SEGMENTS_PER_FILE` (4096). Tests only; the parser is untouched.
//!
//! Contract under test:
//!
//! - `MAX_FILES == 1024` and `MAX_SEGMENTS_PER_FILE == 4096` (literal pins).
//! - A document with `MAX_FILES + 1` files is `Error::Bounds` with
//!   `bound == "file count"`; the exact-bound (1024-file) input parses.
//! - A file with `MAX_SEGMENTS_PER_FILE + 1` segments is `Error::Bounds`
//!   with `bound == "segments in file"`; the exact-bound (4096-segment)
//!   input parses.
//!
//! Sensitivity: lowering either literal flips the pin — the at-bound parse
//! becomes a `Bounds` rejection and the `*_are_pinned` literal assertion
//! fails; raising either literal turns the over-bound rejection into a
//! parse success. All fixtures are built byte-for-byte in this file and
//! parsed only through the public `nzb::{parse, MAX_FILES,
//! MAX_SEGMENTS_PER_FILE}` seam plus `Error` for variant matching.

use nntune_core::error::Error;
use nntune_core::nzb::{parse, MAX_FILES, MAX_SEGMENTS_PER_FILE};

/// Local literals: the bounds under test. Fixtures below are built from
/// these, not from the crate constants, so a bound-value change fails the
/// `bounds_are_pinned` assertion instead of silently following the code.
const FILE_CAP: usize = 1024;
const SEGMENTS_CAP: usize = 4096;

/// One valid file body with `segment_count` contiguous segments. Message-IDs
/// carry `file_ordinal` so multi-file documents stay duplicate-free for the
/// segment-digest check.
fn file_body(file_ordinal: usize, segment_count: usize, segment_offset: usize) -> String {
    let mut s = format!("<file poster=\"p\" date=\"1\" subject=\"s{file_ordinal}\"><segments>");
    for i in 0..segment_count {
        let number = segment_offset + i + 1;
        s.push_str(&format!(
            "<segment bytes=\"1\" number=\"{number}\">f{file_ordinal}-s{number}@example.com</segment>"
        ));
    }
    s.push_str("</segments></file>");
    s
}

fn doc_with_file_count(count: usize) -> Vec<u8> {
    let mut doc = String::from("<nzb>");
    for i in 0..count {
        doc.push_str(&file_body(i, 1, 0));
    }
    doc.push_str("</nzb>");
    doc.into_bytes()
}

fn doc_with_segment_count(count: usize) -> Vec<u8> {
    let mut doc = String::from("<nzb>");
    doc.push_str(&file_body(0, count, 0));
    doc.push_str("</nzb>");
    doc.into_bytes()
}

fn expect_bounds(err: Error, bound: &str, limit: i64) {
    match err {
        Error::Bounds {
            bound: got,
            limit: got_limit,
        } => {
            assert_eq!(got, bound, "wrong bound name: {got:?}");
            assert_eq!(got_limit, limit, "wrong bound limit: {got_limit}");
        }
        other => panic!("expected Bounds({bound:?}/{limit}), got {other:?}"),
    }
}

#[test]
fn bounds_are_pinned() {
    assert_eq!(FILE_CAP, 1024, "local file-count literal must be 1024");
    assert_eq!(
        SEGMENTS_CAP, 4096,
        "local segments-per-file literal must be 4096"
    );
    assert_eq!(
        MAX_FILES, FILE_CAP,
        "crate file cap moved; this pin exists to catch exactly that"
    );
    assert_eq!(
        MAX_SEGMENTS_PER_FILE, SEGMENTS_CAP,
        "crate segments-per-file cap moved; this pin exists to catch exactly that"
    );
}

#[test]
fn file_count_one_over_cap_is_rejected_with_bounds() {
    let err = parse(&doc_with_file_count(FILE_CAP + 1)).unwrap_err();
    expect_bounds(err, "file count", FILE_CAP as i64);
}

#[test]
fn file_count_at_cap_parses() {
    let doc = parse(&doc_with_file_count(FILE_CAP)).expect("at-bound file count must parse");
    assert_eq!(doc.files.len(), FILE_CAP);
    assert_eq!(doc.files[FILE_CAP - 1].segments.len(), 1);
}

#[test]
fn segments_per_file_one_over_cap_is_rejected_with_bounds() {
    let err = parse(&doc_with_segment_count(SEGMENTS_CAP + 1)).unwrap_err();
    expect_bounds(err, "segments in file", SEGMENTS_CAP as i64);
}

#[test]
fn segments_per_file_at_cap_parses() {
    let doc =
        parse(&doc_with_segment_count(SEGMENTS_CAP)).expect("at-bound segment count must parse");
    assert_eq!(doc.files.len(), 1);
    assert_eq!(doc.files[0].segments.len(), SEGMENTS_CAP);
    assert_eq!(doc.files[0].segments[0].number, 1);
    assert_eq!(doc.files[0].segments[SEGMENTS_CAP - 1].number, SEGMENTS_CAP);
}
