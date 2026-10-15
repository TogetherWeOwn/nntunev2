//! Acceptance tests for [TOG-25180](/TOG/issues/TOG-25180) (roadmap R5 from
//! [TOG-25148](/TOG/issues/TOG-25148)): large-NZB stress corpus manifest.
//!
//! Builder contract (implement in
//! `crates/nntune-core/src/nzb/stress.rs`, exposed as
//! `pub mod stress` in `crates/nntune-core/src/nzb.rs`):
//!
//! ```rust,ignore
//! use std::io::BufRead;
//!
//! pub struct StressCorpusManifest {
//!     pub name: &'static str,
//!     /// `<file>` elements in the synthetic corpus.
//!     pub files: usize,
//!     /// `<segment>` elements per file.
//!     pub segments_per_file: usize,
//!     pub total_segments: usize,
//!     /// Sum of declared `bytes=` — must reach the 2 GiB bar.
//!     pub encoded_bytes: u64,
//!     /// Actual NZB document bytes served to the parser (streamed, bounded).
//!     pub document_bytes: u64,
//!     /// CI wall-clock bound for generate + parse, seconds.
//!     pub time_bound_secs: u64,
//! }
//!
//! pub const STRESS_CORPUS_MANIFEST: StressCorpusManifest;
//! pub const STRESS_TIME_BOUND_SECS: u64;
//!
//! /// Streaming reader for the synthetic NZB: yields the full corpus WITHOUT
//! /// materialising a 2 GiB-equivalent document in memory. Must stay within
//! /// `MAX_DOCUMENT_BYTES` wire bytes while declaring 2 GiB+ of segments
//! /// (repetition + virtual segments, fully deterministic).
//! pub fn stress_nzb_reader() -> impl BufRead;
//! ```
//!
//! Why a reader, not a `Vec<u8>`: a 2 GiB-equivalent corpus as one vector
//! would OOM CI. The Builder must stream (constant memory); a test that
//! times out or OOMs on a materialising implementation is CORRECTLY red.
//!
//! Failing command:
//! `cargo test -p nntune-core --test par2_nzb_stress_corpus`
//! (also runs under `cargo test -p nntune-core par2`)
//!
//! RED reason (2026-10-15): `nntune_core::nzb::stress` does not exist yet,
//! so this target fails to compile with `unresolved import` (E0432). The
//! Builder on [TOG-25180](/TOG/issues/TOG-25180) turns it green without
//! editing this file. Synthetic fixtures only, no provider.

use std::time::Instant;

use nntune_core::nzb::stress::{
    stress_nzb_reader, STRESS_CORPUS_MANIFEST, STRESS_TIME_BOUND_SECS,
};
use nntune_core::nzb::{digest_message_ids, Parser, MAX_DOCUMENT_BYTES};

/// 2 GiB in bytes: the stress bar from the build card.
const TWO_GIB: u64 = 2_147_483_648;

#[test]
fn manifest_declares_two_gibibyte_equivalent_with_bounded_document() {
    let m = &STRESS_CORPUS_MANIFEST;
    assert!(
        m.encoded_bytes >= TWO_GIB,
        "stress corpus must declare >= 2 GiB of encoded segments, got {}",
        m.encoded_bytes
    );
    assert!(m.files > 0, "manifest must declare at least one file");
    assert!(
        m.segments_per_file > 0,
        "manifest must declare at least one segment per file"
    );
    assert_eq!(
        m.total_segments,
        m.files * m.segments_per_file,
        "total_segments must equal files * segments_per_file"
    );
    assert!(
        m.document_bytes <= MAX_DOCUMENT_BYTES as u64,
        "streamed document ({} bytes) must fit the parser bound ({MAX_DOCUMENT_BYTES})",
        m.document_bytes
    );
    assert!(
        m.time_bound_secs > 0 && m.time_bound_secs <= 300,
        "time bound ({}s) must be a positive CI-plausible value (<= 300s)",
        m.time_bound_secs
    );
    assert_eq!(
        m.time_bound_secs, STRESS_TIME_BOUND_SECS,
        "manifest bound and exported const must agree"
    );
}

#[test]
fn generated_corpus_parses_within_time_bound_and_covers_all_segments() {
    let bound = std::time::Duration::from_secs(STRESS_TIME_BOUND_SECS);
    let start = Instant::now();
    let reader = stress_nzb_reader();
    let doc = Parser::with_default_limits()
        .expect("default limits construct")
        .parse(reader)
        .expect("synthetic stress corpus must parse under default limits");
    let elapsed = start.elapsed();

    assert!(
        elapsed < bound,
        "generate + parse took {elapsed:?}, exceeding the {bound:?} CI bound"
    );
    assert_eq!(
        doc.files.len(),
        STRESS_CORPUS_MANIFEST.files,
        "parsed file count must match the manifest"
    );
    let total_segments: usize = doc.files.iter().map(|f| f.segments.len()).sum();
    assert_eq!(
        total_segments, STRESS_CORPUS_MANIFEST.total_segments,
        "parsed segment count must match the manifest"
    );
    assert!(
        doc.encoded_bytes >= TWO_GIB as i64,
        "parsed encoded bytes ({}) must reach the 2 GiB bar",
        doc.encoded_bytes
    );

    // The manifest's corpus is fully addressable: first and last segments of
    // the first and last files carry well-formed message-ids.
    let first = &doc.files.first().expect("at least one file").segments;
    let last = &doc.files.last().expect("at least one file").segments;
    assert!(!first.first().expect("segment").message_id.is_empty());
    assert!(!last.last().expect("segment").message_id.is_empty());

    // The whole-corpus digest covers exactly the parsed segment set.
    let mut ids: Vec<String> = doc
        .files
        .iter()
        .flat_map(|f| f.segments.iter().map(|s| s.message_id.clone()))
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(
        ids.len(),
        STRESS_CORPUS_MANIFEST.total_segments,
        "message-ids must be unique across the corpus (v1 digest rule)"
    );
    let _ = digest_message_ids(&ids).expect("corpus digest must compute");
    assert_eq!(doc.segment_digest, digest_message_ids(&ids).unwrap());
}

#[test]
fn corpus_generation_is_deterministic_across_runs() {
    // CI must be able to re-run the corpus and get the same bytes: read the
    // streamed document twice and compare digests.
    let doc_a = Parser::with_default_limits()
        .unwrap()
        .parse(stress_nzb_reader())
        .expect("first generation parses");
    let doc_b = Parser::with_default_limits()
        .unwrap()
        .parse(stress_nzb_reader())
        .expect("second generation parses");
    assert_eq!(
        doc_a.segment_digest, doc_b.segment_digest,
        "stress corpus must be deterministic across runs"
    );
    assert_eq!(
        doc_a.encoded_bytes, doc_b.encoded_bytes,
        "encoded byte totals must be stable across runs"
    );
}
