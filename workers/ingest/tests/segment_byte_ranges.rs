//! S9 acceptance: per-segment byte ranges in parse/probe envelopes.
//!
//! Test-first pair for slice S9 of the roadmap. This file is owned by the
//! acceptance-test work: the implementation lands alongside it and must
//! make these tests pass without editing them.
//!
//! ## Expected post-S9 API (the Builder implements exactly this)
//!
//! - `parse::ParsedNzb` gains two additive fields, both `None` = explicit
//!   unknown (never zero-filled):
//!   ```ignore
//!   pub segment_bytes: Vec<Option<u64>>,       // flat, parallel to message_ids
//!   pub file_segment_bytes: Vec<Vec<Option<u64>>>, // per-file, NZB order
//!   ```
//! - `parse::parse_segment_byte_ranges(nzb_xml: &str)
//!     -> (Vec<Option<u64>>, Vec<Vec<Option<u64>>>)`
//!   returns `(flat_in_document_order, per_file_in_nzb_order)` sourced from
//!   each `<segment bytes="N">`. Missing, non-numeric, zero or negative
//!   `bytes=` yields `None` (explicit-unknown), never `Some(0)`.
//! - `parse::probe_batches_with_ranges(&ParsedNzb)
//!     -> Vec<Vec<(String, Option<u64>)>>`
//!   chunks `(message_id, bytes)` pairs in NZB order, capped at
//!   `MAX_PROBE_SEGMENTS` per batch (same order/cap as `probe_batches`).
//! - `probe::ProbeRequest` gains `pub segment_bytes: Vec<Option<u64>>`,
//!   parallel to `segment_ids`.
//! - `probe::build_probe_request_with_ranges(
//!     msg: &QueueMsg, batch_index: usize, segments: &[(&str, Option<u64>)],
//!   )` validates ids exactly like `build_probe_request` and carries the
//!   bytes through parallel.
//! - `parse::check_parsed` additionally rejects range-length mismatches
//!   (`segment_bytes.len() != segment_count`, flattened per-file mismatch)
//!   and rejects `Some(0)` (zero-filled) ranges.
//!
//! All of the above fail to compile on the pre-S9 head (missing fields and
//! functions): that compile failure IS the failing acceptance signal. The
//! Builder makes exactly these tests pass without editing this file.

use nntune_ingest::dispatch::build_upload_dispatch;
use nntune_ingest::parse::{
    check_parsed, parse_segment_byte_ranges, probe_batches_with_ranges, ParsedNzb,
};
use nntune_ingest::probe::{build_probe_request_with_ranges, MAX_PROBE_SEGMENTS};

/// Two files, three segments: file 1 holds 100 + 200 byte segments, file 2
/// holds one 300 byte segment. Document order is a1, a2, b1.
const MULTI_FILE_NZB: &str = r#"<?xml version="1.0"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
<file poster="a@b" date="1" subject="album-track01.flac">
<groups><group>alt.binaries.music</group></groups>
<segments>
<segment bytes="100" number="1">seg-a1@synthetic.invalid</segment>
<segment bytes="200" number="2">seg-a2@synthetic.invalid</segment>
</segments>
</file>
<file poster="a@b" date="2" subject="album-track02.flac">
<groups><group>alt.binaries.music</group></groups>
<segments>
<segment bytes="300" number="1">seg-b1@synthetic.invalid</segment>
</segments>
</file>
</nzb>"#;

/// Same shape, no `bytes=` metadata at all: every range must be explicit
/// unknown (`None`), never zero-filled (`Some(0)`).
const NO_RANGE_NZB: &str = r#"<?xml version="1.0"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
<file poster="a@b" date="1" subject="track03.flac">
<groups><group>alt.binaries.music</group></groups>
<segments>
<segment number="1">seg-c1@synthetic.invalid</segment>
<segment number="2">seg-c2@synthetic.invalid</segment>
</segments>
</file>
</nzb>"#;

/// Mixed: one segment declares 150 bytes, the other declares nothing, one
/// declares an invalid zero. All unknown positions must be `None`.
const MIXED_RANGE_NZB: &str = r#"<?xml version="1.0"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
<file poster="a@b" date="1" subject="track04.flac">
<groups><group>alt.binaries.music</group></groups>
<segments>
<segment bytes="150" number="1">seg-d1@synthetic.invalid</segment>
<segment number="2">seg-d2@synthetic.invalid</segment>
<segment bytes="0" number="3">seg-d3@synthetic.invalid</segment>
</segments>
</file>
</nzb>"#;

fn parsed_multi_file() -> ParsedNzb {
    ParsedNzb {
        file_count: 2,
        segment_count: 3,
        message_ids: vec![
            "seg-a1@synthetic.invalid".to_string(),
            "seg-a2@synthetic.invalid".to_string(),
            "seg-b1@synthetic.invalid".to_string(),
        ],
        segment_bytes: vec![Some(100), Some(200), Some(300)],
        file_segment_bytes: vec![vec![Some(100), Some(200)], vec![Some(300)]],
    }
}

#[test]
fn multi_segment_fixture_yields_correct_flat_and_per_file_ranges() {
    let (flat, per_file) = parse_segment_byte_ranges(MULTI_FILE_NZB);
    assert_eq!(flat, vec![Some(100), Some(200), Some(300)]);
    assert_eq!(per_file, vec![vec![Some(100), Some(200)], vec![Some(300)]]);
    // Per-file flattens to the flat list in document order: the stream
    // range-seek path joins files back in exactly this order.
    let joined: Vec<Option<u64>> = per_file.into_iter().flatten().collect();
    assert_eq!(joined, flat);

    // The parsed envelope built from those ranges passes the intake
    // cross-check (2 files / 3 segments agree with the intake guard).
    let parsed = parsed_multi_file();
    let est = nntune_ingest::nzb_intake::estimate_nzb(MULTI_FILE_NZB).unwrap();
    assert_eq!((est.file_count, est.segment_count), (2, 3));
    assert!(check_parsed(&est, &parsed).is_ok());
}

#[test]
fn missing_range_metadata_yields_explicit_unknown_never_zero_filled() {
    let (flat, per_file) = parse_segment_byte_ranges(NO_RANGE_NZB);
    assert_eq!(flat, vec![None, None]);
    assert_eq!(per_file, vec![vec![None, None]]);
    // Explicit-unknown means literally None: a zero-filled Some(0) would
    // send the stream path to fetch an empty range.
    assert!(
        !flat.contains(&Some(0)),
        "missing bytes= must never become Some(0)"
    );

    let (mixed_flat, mixed_per_file) = parse_segment_byte_ranges(MIXED_RANGE_NZB);
    assert_eq!(mixed_flat, vec![Some(150), None, None]);
    assert_eq!(mixed_per_file, vec![vec![Some(150), None, None]]);
    assert!(
        !mixed_flat.contains(&Some(0)),
        "bytes=\"0\" is not a known range: must be None, never Some(0)"
    );
}

#[test]
fn parsed_nzb_carries_ranges_parallel_to_message_ids() {
    let parsed = parsed_multi_file();
    // Parallel arrays: every message-id has exactly one range slot.
    assert_eq!(parsed.message_ids.len(), parsed.segment_bytes.len());
    assert_eq!(parsed.message_ids.len(), parsed.segment_count);
    assert_eq!(
        parsed
            .file_segment_bytes
            .iter()
            .map(|f| f.len())
            .sum::<usize>(),
        parsed.segment_count
    );
    // Spot values, not just lengths: the envelope must source them from the
    // NZB bytes= attributes, not default them.
    assert_eq!(parsed.segment_bytes[0], Some(100));
    assert_eq!(parsed.segment_bytes[1], Some(200));
    assert_eq!(parsed.segment_bytes[2], Some(300));

    // Serde round-trip (checkpoint persistence) preserves known vs unknown.
    let json = serde_json::to_string(&parsed).unwrap();
    let back: ParsedNzb = serde_json::from_str(&json).unwrap();
    assert_eq!(back, parsed);
    let unknown = ParsedNzb {
        file_count: 1,
        segment_count: 2,
        message_ids: vec![
            "seg-c1@synthetic.invalid".to_string(),
            "seg-c2@synthetic.invalid".to_string(),
        ],
        segment_bytes: vec![None, None],
        file_segment_bytes: vec![vec![None, None]],
    };
    let back_unknown: ParsedNzb =
        serde_json::from_str(&serde_json::to_string(&unknown).unwrap()).unwrap();
    assert_eq!(back_unknown.segment_bytes, vec![None, None]);
}

#[test]
fn probe_batches_carry_ranges_end_to_end() {
    let parsed = parsed_multi_file();
    let batches = probe_batches_with_ranges(&parsed);
    assert_eq!(batches.len(), 1);
    assert_eq!(
        batches[0],
        vec![
            ("seg-a1@synthetic.invalid".to_string(), Some(100)),
            ("seg-a2@synthetic.invalid".to_string(), Some(200)),
            ("seg-b1@synthetic.invalid".to_string(), Some(300)),
        ]
    );

    // End to end: every batch builds a probe envelope carrying its ranges.
    let msg = build_upload_dispatch("user_01", MULTI_FILE_NZB).unwrap();
    for (batch_index, batch) in batches.iter().enumerate() {
        let refs: Vec<(&str, Option<u64>)> = batch
            .iter()
            .map(|(id, bytes)| (id.as_str(), *bytes))
            .collect();
        let req = build_probe_request_with_ranges(&msg, batch_index, &refs).unwrap();
        assert_eq!(req.batch_index, batch_index);
        assert_eq!(
            req.segment_ids,
            batch.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>()
        );
        assert_eq!(
            req.segment_bytes,
            batch.iter().map(|(_, b)| *b).collect::<Vec<_>>()
        );
    }

    // Unknown ranges ride the same path: None in, None out.
    let unknown_refs: Vec<(&str, Option<u64>)> = vec![
        ("seg-c1@synthetic.invalid", None),
        ("seg-c2@synthetic.invalid", None),
    ];
    let unknown_req = build_probe_request_with_ranges(&msg, 0, &unknown_refs).unwrap();
    assert_eq!(unknown_req.segment_bytes, vec![None, None]);

    // Large parses respect the probe cap with ranges attached: 501 segments
    // split into 500 + 1, ranges intact and in order.
    let many_ids: Vec<String> = (0..(MAX_PROBE_SEGMENTS + 1))
        .map(|i| format!("seg-{i:05}@synthetic.invalid"))
        .collect();
    let many_bytes: Vec<Option<u64>> = (0..(MAX_PROBE_SEGMENTS + 1))
        .map(|i| Some(100 + i as u64))
        .collect();
    let many = ParsedNzb {
        file_count: 1,
        segment_count: many_ids.len(),
        message_ids: many_ids.clone(),
        segment_bytes: many_bytes.clone(),
        file_segment_bytes: vec![many_bytes.clone()],
    };
    let many_batches = probe_batches_with_ranges(&many);
    assert_eq!(many_batches.len(), 2);
    assert!(many_batches.iter().all(|b| b.len() <= MAX_PROBE_SEGMENTS));
    let joined: Vec<(String, Option<u64>)> = many_batches.into_iter().flatten().collect();
    let expected: Vec<(String, Option<u64>)> = many_ids.into_iter().zip(many_bytes).collect();
    assert_eq!(joined, expected);
}

#[test]
fn check_parsed_rejects_range_length_mismatch_and_zero_filled() {
    let est = nntune_ingest::nzb_intake::estimate_nzb(MULTI_FILE_NZB).unwrap();
    // Flat length must match the segment count.
    let mut short_flat = parsed_multi_file();
    short_flat.segment_bytes.pop();
    assert!(check_parsed(&est, &short_flat).is_err());
    // Per-file flattened length must match too.
    let mut short_file = parsed_multi_file();
    short_file.file_segment_bytes[0].pop();
    assert!(check_parsed(&est, &short_file).is_err());
    // Zero-filled is never a valid range: missing metadata is None, and a
    // Some(0) that slipped through must fail the cross-check rather than
    // spend provider connections on an empty fetch.
    let mut zero_filled = parsed_multi_file();
    zero_filled.segment_bytes[0] = Some(0);
    assert!(check_parsed(&est, &zero_filled).is_err());
    // The envelope validator still guards the ids on the ranged path.
    let msg = build_upload_dispatch("user_01", MULTI_FILE_NZB).unwrap();
    assert!(build_probe_request_with_ranges(
        &msg,
        0,
        &[("has space@synthetic.invalid", Some(100))]
    )
    .is_err());
    assert!(build_probe_request_with_ranges(&msg, 0, &[]).is_err());
}

#[test]
fn range_scanner_ignores_comments_and_matches_prefixed_segment_names() {
    let xml = r#"<nzb xmlns:n="urn:nzb"><file>
<!-- <segment bytes="18446744073709551615" number="1">fake@invalid</segment> -->
<n:segment bytes="111" number="1">real@invalid</n:segment>
</file></nzb>"#;
    let (flat, per_file) = parse_segment_byte_ranges(xml);
    assert_eq!(flat, vec![Some(111)]);
    assert_eq!(per_file, vec![vec![Some(111)]]);
}

#[test]
fn invalid_or_unbounded_ranges_are_unknown_or_rejected() {
    let xml = r#"<nzb><file><segment bytes="18446744073709551615" number="1">x@y</segment></file></nzb>"#;
    let (flat, _) = parse_segment_byte_ranges(xml);
    assert_eq!(flat, vec![None]);

    let est = nntune_ingest::nzb_intake::estimate_nzb(MULTI_FILE_NZB).unwrap();
    let mut oversized = parsed_multi_file();
    oversized.segment_bytes[0] = Some(2_147_483_648);
    oversized.file_segment_bytes[0][0] = Some(2_147_483_648);
    assert!(check_parsed(&est, &oversized).is_err());
    let segment_count = 33;
    let excessive_total = ParsedNzb {
        file_count: 1,
        segment_count,
        message_ids: (0..segment_count)
            .map(|i| format!("seg-{i}@synthetic.invalid"))
            .collect(),
        segment_bytes: vec![Some(2_147_483_647); segment_count],
        file_segment_bytes: vec![vec![Some(2_147_483_647); segment_count]],
    };
    let estimate = nntune_ingest::nzb_intake::IntakeEstimate {
        file_count: 1,
        segment_count,
    };
    assert!(check_parsed(&estimate, &excessive_total).is_err());

    let msg = build_upload_dispatch("user_01", MULTI_FILE_NZB).unwrap();
    assert!(build_probe_request_with_ranges(&msg, 0, &[("x@y", Some(0))]).is_err());
    assert!(build_probe_request_with_ranges(&msg, 0, &[("x@y", Some(2_147_483_648))]).is_err());
}

#[test]
fn file_blocks_ignore_commented_out_file_markup() {
    let xml = r#"<nzb><file poster="a@b" date="1" subject="t1">
<segments><segment bytes="100" number="1">a@x.invalid</segment></segments></file>
<!-- <file poster="a@b" date="2" subject="t2">
<segments><segment bytes="200" number="1">b@x.invalid</segment></segments></file> -->
</nzb>"#;
    let (flat, per_file) = parse_segment_byte_ranges(xml);
    assert_eq!(flat, vec![Some(100)]);
    assert_eq!(per_file, vec![vec![Some(100)]]);
}

#[test]
fn ranged_probe_batches_reject_mismatched_parallel_arrays() {
    let mut parsed = parsed_multi_file();
    parsed.segment_bytes.pop();
    assert!(std::panic::catch_unwind(|| probe_batches_with_ranges(&parsed)).is_err());
}
