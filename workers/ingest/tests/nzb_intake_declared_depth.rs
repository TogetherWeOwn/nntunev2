//! NZB intake declared-bytes cap + well-formedness depth-bound pins
//! (`workers/ingest/src/nzb_intake.rs`).
//!
//! Contract under test:
//!
//! - [`DEFAULT_LIMITS`] is the route policy: `max_declared_bytes == 64 GiB`
//!   (`68_719_476_736`, [`MAX_DECLARED_BYTES`]).
//! - Two segments declaring 40 GiB each under [`DEFAULT_LIMITS`] are rejected
//!   with [`IntakeRejection::DeclaredBytesOverCap`] carrying
//!   `declared == 85_899_345_920`, code `NZB_DECLARED_BYTES_OVER_CAP` and
//!   HTTP 413. The payload is a few hundred bytes of XML with two segments,
//!   so neither the size cap nor the count caps fire first.
//! - An unparseable `bytes=` (non-numeric, empty) is [`IntakeRejection::Malformed`]
//!   (400), never a wrap/panic: the summing scan fails closed.
//! - Nesting with 1_022 `<a>` elements between `<nzb>` and `<segment>` passes
//!   (1_024 open elements total: the bound counts `<nzb>` and the leaf
//!   `<segment>`); 1_023 `<a>` elements is [`IntakeRejection::Malformed`].
//!   The everyday ~4-deep shape (`nzb > file > segments > segment`) passes.
//!
//! File/segment count caps are pinned separately in
//! `tests/nzb_intake_default_count_caps.rs`; route envelope wiring for the
//! same over-bytes shape lives in `src/lib.rs`
//! (`intake_corpus_maps_to_registered_envelopes_with_reason_fields`). In-file
//! unit tests only exercise custom limits; this integration file is the
//! default-cap + depth-bound pin.

use nntune_ingest::nzb_intake::{
    estimate_nzb, estimate_nzb_with_limits, IntakeRejection, DEFAULT_LIMITS, MAX_DECLARED_BYTES,
    MAX_NZB_BYTES,
};

const FORTY_GIB: u64 = 40 * 1024 * 1024 * 1024;
const SIXTY_FOUR_GIB: u64 = 64 * 1024 * 1024 * 1024;
const TWO_FORTY_GIB: u64 = 2 * FORTY_GIB;

fn over_bytes_doc() -> String {
    format!(
        "<?xml?><nzb><file><segments>\
<segment bytes=\"{FORTY_GIB}\" number=\"1\">big-one@news</segment>\
<segment bytes=\"{FORTY_GIB}\" number=\"2\">big-two@news</segment>\
</segments></file></nzb>"
    )
}

fn unparseable_doc(bytes_value: &str) -> String {
    format!("<?xml?><nzb><segment bytes=\"{bytes_value}\">x@y</segment></nzb>")
}

fn nesting_doc(depth: usize) -> String {
    let mut doc = String::from("<?xml?><nzb>");
    for _ in 0..depth {
        doc.push_str("<a>");
    }
    doc.push_str("<segment>x@y</segment>");
    for _ in 0..depth {
        doc.push_str("</a>");
    }
    doc.push_str("</nzb>");
    doc
}

#[test]
fn default_limits_pin_declared_bytes_cap() {
    assert_eq!(DEFAULT_LIMITS.max_declared_bytes, SIXTY_FOUR_GIB);
    assert_eq!(DEFAULT_LIMITS.max_declared_bytes, MAX_DECLARED_BYTES);
    assert_eq!(DEFAULT_LIMITS.max_declared_bytes, 68_719_476_736);
}

#[test]
fn two_40gib_segments_rejected_with_exact_declared() {
    assert_eq!(FORTY_GIB, 42_949_672_960);
    assert_eq!(TWO_FORTY_GIB, 85_899_345_920);
    let doc = over_bytes_doc();
    assert!(doc.len() < MAX_NZB_BYTES);
    let rejected = estimate_nzb(&doc).unwrap_err();
    assert_eq!(rejected.code(), "NZB_DECLARED_BYTES_OVER_CAP");
    assert_eq!(rejected.http_status(), 413);
    assert_eq!(
        rejected,
        IntakeRejection::DeclaredBytesOverCap {
            declared: 85_899_345_920,
            max: 68_719_476_736,
        }
    );
    // Same verdict through the explicit route policy object.
    assert_eq!(
        estimate_nzb_with_limits(&doc, &DEFAULT_LIMITS).unwrap_err(),
        rejected
    );
}

#[test]
fn at_cap_declared_bytes_pass_under_default_limits() {
    let doc = format!("<?xml?><nzb><segment bytes=\"{SIXTY_FOUR_GIB}\">x@y</segment></nzb>");
    let est = estimate_nzb(&doc).unwrap();
    assert_eq!(est.declared_bytes, 68_719_476_736);
    assert_eq!(
        estimate_nzb_with_limits(&doc, &DEFAULT_LIMITS)
            .unwrap()
            .declared_bytes,
        68_719_476_736
    );
}

#[test]
fn unparseable_bytes_is_malformed_without_wrap_or_panic() {
    for value in ["lots", "", "12.5", "0x10", "-5", "1e3"] {
        let doc = unparseable_doc(value);
        let rejected = estimate_nzb(&doc).unwrap_err();
        assert_eq!(rejected, IntakeRejection::Malformed, "bytes={value:?}");
        assert_eq!(rejected.code(), "NZB_MALFORMED");
        assert_eq!(rejected.http_status(), 400);
        assert_eq!(
            estimate_nzb_with_limits(&doc, &DEFAULT_LIMITS).unwrap_err(),
            IntakeRejection::Malformed,
            "bytes={value:?} through DEFAULT_LIMITS"
        );
    }
}

#[test]
fn nesting_1025_deep_is_malformed_while_4_deep_passes() {
    // Everyday NZB shape nests ~4 deep (nzb > file > segments > segment).
    let shallow =
        "<?xml?><nzb><file><segments><segment bytes=\"100\">x@y</segment></segments></file></nzb>";
    let est = estimate_nzb(shallow).unwrap();
    assert_eq!(est.segment_count, 1);
    assert_eq!(est.declared_bytes, 100);

    // The 1_024 well-formedness bound counts <nzb> and the leaf <segment>,
    // so 1_022 intermediate elements are the last accepted shape and 1_023
    // is the first rejected shape; each payload is a few KB, so the depth
    // gate (not any size/count cap) is what fires.
    for (depth, ok) in [(1_022, true), (1_023, false)] {
        let doc = nesting_doc(depth);
        assert!(doc.len() < MAX_NZB_BYTES);
        if ok {
            assert!(
                estimate_nzb(&doc).is_ok(),
                "depth {depth} (1_022 intermediates) should pass",
            );
            assert!(
                estimate_nzb_with_limits(&doc, &DEFAULT_LIMITS).is_ok(),
                "depth {depth} through DEFAULT_LIMITS should pass",
            );
        } else {
            assert_eq!(
                estimate_nzb(&doc).unwrap_err(),
                IntakeRejection::Malformed,
                "depth {depth} (1_023 intermediates) should be malformed",
            );
            assert_eq!(
                estimate_nzb_with_limits(&doc, &DEFAULT_LIMITS).unwrap_err(),
                IntakeRejection::Malformed,
                "depth {depth} through DEFAULT_LIMITS should be malformed",
            );
        }
    }
    let deep = nesting_doc(1_025);
    assert!(deep.len() < MAX_NZB_BYTES);
    let rejected = estimate_nzb(&deep).unwrap_err();
    assert_eq!(rejected, IntakeRejection::Malformed);
    assert_eq!(rejected.code(), "NZB_MALFORMED");
    assert_eq!(rejected.http_status(), 400);
    assert_eq!(
        estimate_nzb_with_limits(&deep, &DEFAULT_LIMITS).unwrap_err(),
        IntakeRejection::Malformed
    );
}
