//! Segment-text accumulation-cap pin (`crates/nntune-core/src/nzb.rs:44`
//! `push_segment_text`, `MAX_SEGMENT_TEXT_BYTES`).
///
/// Contract under test:
///
/// - a `<segment>` whose accumulated character data is one byte over
///   [`MAX_SEGMENT_TEXT_BYTES`] (1 MiB) is rejected with
///   `segment text is too large`;
/// - the same over-cap text fed through a `BufRead` that yields at most
///   64 KiB per `fill_buf` is still rejected with the same message (the cap
///   is on the accumulation across `Text`/`CData`/`GeneralRef` events, not
///   on a single transport chunk);
/// - a small valid NZB still parses (positive control).
///
/// Sensitivity: deleting the accumulation check (or checking only
/// `chunk.len()` instead of `text.len() + chunk.len()`) lets the 64 KiB
/// chunked over-cap payload through to `normalize_message_id`, which fails
/// with a different message (`at most 998 bytes`) or — for a short-cap
/// mutant — parses outright. Asserting the exact `segment text is too large`
/// wording kills both mutants. Uses only the public
/// `nzb::{Parser, Limits}` surface; the implementation is untouched.
use std::io::{self, BufRead, Read};

use nntune_core::nzb::{Limits, Parser, MAX_SEGMENT_TEXT_BYTES};

fn doc_with_segment_text(inner: &str) -> Vec<u8> {
    format!(
        "<nzb><file poster=\"p\" date=\"1\" subject=\"s\"><segments>\
         <segment bytes=\"1\" number=\"1\">{inner}</segment>\
         </segments></file></nzb>"
    )
    .into_bytes()
}

/// `BufRead` adapter that caps each `fill_buf` to `cap` bytes, forcing the
/// parser to stream the document in 64 KiB transport chunks.
struct Chunked<R> {
    inner: R,
    cap: usize,
}

impl<R: BufRead> Read for Chunked<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let bytes = self.inner.fill_buf()?;
        let n = bytes.len().min(out.len()).min(self.cap);
        out[..n].copy_from_slice(&bytes[..n]);
        self.inner.consume(n);
        Ok(n)
    }
}

impl<R: BufRead> BufRead for Chunked<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        let bytes = self.inner.fill_buf()?;
        Ok(&bytes[..bytes.len().min(self.cap)])
    }

    fn consume(&mut self, amount: usize) {
        self.inner.consume(amount);
    }
}

fn over_cap_text() -> String {
    "a".repeat(MAX_SEGMENT_TEXT_BYTES + 1)
}

#[test]
fn max_plus_one_segment_text_rejects_with_exact_message() {
    let xml = doc_with_segment_text(&over_cap_text());
    let err = Parser::with_default_limits()
        .unwrap()
        .parse(xml.as_slice())
        .unwrap_err();
    assert!(
        err.to_string().contains("segment text is too large"),
        "expected segment-text cap message, got: {err}"
    );
    // Same verdict through the explicit default-limits constructor, proving
    // the pin goes through the public `Limits` surface.
    let err = Parser::new(Limits::default())
        .unwrap()
        .parse(xml.as_slice())
        .unwrap_err();
    assert!(
        err.to_string().contains("segment text is too large"),
        "expected segment-text cap message via Limits::default, got: {err}"
    );
}

#[test]
fn over_cap_text_split_across_64kib_chunks_still_rejects() {
    let xml = doc_with_segment_text(&over_cap_text());
    let chunked = Chunked {
        inner: xml.as_slice(),
        cap: 64 * 1024,
    };
    let err = Parser::with_default_limits()
        .unwrap()
        .parse(chunked)
        .unwrap_err();
    assert!(
        err.to_string().contains("segment text is too large"),
        "chunked transport must not bypass the accumulation cap, got: {err}"
    );
}

#[test]
fn small_valid_nzb_parses() {
    let xml = doc_with_segment_text("seg-1@example.com");
    let doc = Parser::with_default_limits()
        .unwrap()
        .parse(xml.as_slice())
        .unwrap();
    assert_eq!(doc.files.len(), 1);
    assert_eq!(doc.files[0].segments.len(), 1);
    assert_eq!(doc.files[0].segments[0].message_id, "seg-1@example.com");
}
