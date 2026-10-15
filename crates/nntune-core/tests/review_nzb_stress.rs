//! Large-NZB stress corpus: a 2 GiB+ equivalent synthetic document.
//!
//! The corpus declares gigabytes of encoded bytes while the XML itself stays
//! a few hundred kilobytes: each `<segment>` declares a 1 MiB `bytes=`
//! payload but carries only a short Message-ID. This exercises the parser's
//! resource accounting (declared-byte summation, per-file segment counts,
//! digest over thousands of IDs) at production scale without materialising
//! gigabytes in CI.
//!
//! Stated time bound: [`STRESS_TIME_BOUND`]. Both tests assert it; the
//! measured wall time is an error on breach, never a silent pass.

use std::time::{Duration, Instant};

use nntune_core::nzb::Parser;

/// Stated CI time bound for parsing the full stress corpus (debug build).
const STRESS_TIME_BOUND: Duration = Duration::from_secs(10);

/// Declared wire bytes per synthetic segment (1 MiB of encoded payload).
const BYTES_PER_SEGMENT: i64 = 1 << 20;

fn build_corpus(files: usize, segments_per_file: usize) -> Vec<u8> {
    let mut xml = String::from("<nzb>");
    for f in 0..files {
        xml.push_str(&format!(
            "<file poster=\"p\" date=\"1700000000\" subject=\"stress vol{f} (1/1)\"><segments>"
        ));
        for s in 0..segments_per_file {
            let number = s + 1;
            xml.push_str(&format!(
                "<segment bytes=\"{BYTES_PER_SEGMENT}\" number=\"{number}\">msg-{f}-{s}@stress.local</segment>"
            ));
        }
        xml.push_str("</segments></file>");
    }
    xml.push_str("</nzb>");
    xml.into_bytes()
}

fn parse_timed(xml: &[u8]) -> (nntune_core::nzb::Document, Duration) {
    let started = Instant::now();
    let doc = Parser::with_default_limits()
        .unwrap()
        .parse(xml)
        .expect("stress corpus parses within default limits");
    (doc, started.elapsed())
}

#[test]
fn stress_corpus_of_four_gib_declared_parses_under_time_bound() {
    // 64 files x 64 segments x 1 MiB = 4 GiB declared, ~300 KiB of XML.
    let xml = build_corpus(64, 64);
    assert!(
        (xml.len() as u64) < nntune_core::nzb::MAX_DOCUMENT_BYTES,
        "corpus XML {} bytes must stay under the document bound",
        xml.len()
    );
    let (doc, elapsed) = parse_timed(&xml);
    assert_eq!(doc.files.len(), 64);
    assert!(doc.files.iter().all(|f| f.segments.len() == 64));
    let declared = 64i64 * 64 * BYTES_PER_SEGMENT;
    assert_eq!(doc.encoded_bytes, declared);
    assert!(
        declared >= 2 * 1024 * 1024 * 1024,
        "corpus must declare 2 GiB or more, got {declared}"
    );
    let ids: usize = doc.files.iter().map(|f| f.segments.len()).sum();
    assert_eq!(ids, 4096);
    assert_ne!(doc.segment_digest, [0u8; 32]);
    assert!(
        elapsed <= STRESS_TIME_BOUND,
        "stress parse took {elapsed:?}, bound is {STRESS_TIME_BOUND:?} ({} KiB XML, {} segments)",
        xml.len() / 1024,
        ids,
    );
}

#[test]
fn single_file_two_gib_declared_parses_under_time_bound() {
    // 1 file x 2048 segments x 1 MiB = 2 GiB declared: the segments-per-file
    // scaling axis, still under the 4096 cap.
    let xml = build_corpus(1, 2048);
    let (doc, elapsed) = parse_timed(&xml);
    assert_eq!(doc.files.len(), 1);
    assert_eq!(doc.files[0].segments.len(), 2048);
    assert_eq!(doc.encoded_bytes, 2048 * BYTES_PER_SEGMENT);
    assert!(
        elapsed <= STRESS_TIME_BOUND,
        "stress parse took {elapsed:?}, bound is {STRESS_TIME_BOUND:?}"
    );
}
