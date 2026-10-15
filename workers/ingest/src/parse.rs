//! Parse glue: the parsed shape the checkpoint stores.
//!
//! Full NZB parsing (subject decoding, yEnc segment maps) is owned by the
//! nntune-core card; this module owns the ingest side: the [`ParsedNzb`]
//! shape the `Parsing` step must produce, the cross-check that keeps a parse
//! disagreeing with the intake guard from spending provider connections, and
//! the batching into probe-sized envelopes.

use serde::{Deserialize, Serialize};

use crate::nzb_intake::IntakeEstimate;
use crate::probe::{validate_message_id, MAX_PROBE_SEGMENTS};

/// What the `Parsing` step stores in the checkpoint: the segment map the
/// pool probe walks. Produced by `nntune-core` once that card lands; until
/// then tests and the synthetic E2E construct it from fixtures.
///
/// `segment_bytes` runs flat in document order, parallel to `message_ids`;
/// `file_segment_bytes` runs per-file in NZB order. `None` is
/// explicit-unknown (the NZB carried no usable `bytes=` for that segment),
/// never zero-filled: a `Some(0)` would send the stream path to fetch an
/// empty range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedNzb {
    pub file_count: usize,
    pub segment_count: usize,
    pub message_ids: Vec<String>,
    pub segment_bytes: Vec<Option<u64>>,
    pub file_segment_bytes: Vec<Vec<Option<u64>>>,
}

/// Cross-check a parse against the intake guard's estimate. Any disagreement
/// fails the job before a single provider connection is spent.
pub fn check_parsed(estimate: &IntakeEstimate, parsed: &ParsedNzb) -> Result<(), &'static str> {
    if parsed.file_count != estimate.file_count || parsed.segment_count != estimate.segment_count {
        return Err("parse disagrees with the intake guard");
    }
    if parsed.message_ids.len() != parsed.segment_count {
        return Err("message-id count must match the segment count");
    }
    if parsed.segment_bytes.len() != parsed.segment_count {
        return Err("segment byte-range count must match the segment count");
    }
    let ranged: usize = parsed.file_segment_bytes.iter().map(|f| f.len()).sum();
    if ranged != parsed.segment_count {
        return Err("per-file byte-range count must match the segment count");
    }
    // Missing metadata is explicit-unknown (None): a zero-filled Some(0)
    // must fail here rather than spend provider connections on an empty
    // fetch downstream.
    if parsed.segment_bytes.contains(&Some(0))
        || parsed
            .file_segment_bytes
            .iter()
            .flatten()
            .any(|b| *b == Some(0))
    {
        return Err("segment byte ranges must be explicit-unknown, never zero-filled");
    }
    // Ids flow into NNTP STAT/ARTICLE lines on the user's provider
    // connection: only strict message-ids pass, so a hostile NZB (or BYO
    // indexer) cannot inject CR/LF commands. Same bar as the probe envelope.
    if parsed.message_ids.iter().any(|id| !validate_message_id(id)) {
        return Err("message-ids must be valid msg-ids (printable ASCII, one @)");
    }
    Ok(())
}

/// Chunk message-ids into probe-sized batches, in NZB order.
pub fn probe_batches(parsed: &ParsedNzb) -> Vec<Vec<String>> {
    parsed
        .message_ids
        .chunks(MAX_PROBE_SEGMENTS)
        .map(|c| c.to_vec())
        .collect()
}

/// Chunk `(message-id, byte-range)` pairs into probe-sized batches, in NZB
/// order. Same order and cap as [`probe_batches`]; ranges ride parallel so
/// the stream path can range-seek without re-reading the NZB.
pub fn probe_batches_with_ranges(parsed: &ParsedNzb) -> Vec<Vec<(String, Option<u64>)>> {
    let pairs: Vec<(String, Option<u64>)> = parsed
        .message_ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            (
                id.clone(),
                parsed.segment_bytes.get(i).copied().unwrap_or(None),
            )
        })
        .collect();
    pairs
        .chunks(MAX_PROBE_SEGMENTS)
        .map(|c| c.to_vec())
        .collect()
}

/// True for bytes that continue an XML element name (mirrors the intake
/// guard's scan so `<segments>` never counts as `<segment>`).
fn is_tag_name_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':')
}

/// Read one `bytes=` value out of a `<segment …>` opening tag. Missing,
/// non-numeric, zero, or negative yields `None` (explicit-unknown), never
/// `Some(0)`. Handles single/double quotes and whitespace around `=`.
fn bytes_attr(open_tag: &str) -> Option<u64> {
    let bytes = open_tag.as_bytes();
    let mut i = 0;
    while i + 5 <= bytes.len() {
        if &bytes[i..i + 5] == b"bytes" {
            let prev_ok = i == 0 || !is_tag_name_char(bytes[i - 1]);
            if prev_ok {
                let mut j = i + 5;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if bytes.get(j).copied() == Some(b'=') {
                    j += 1;
                    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    let quote = bytes.get(j).copied();
                    if quote == Some(b'"') || quote == Some(b'\'') {
                        let q = quote.unwrap_or(b'"');
                        j += 1;
                        let start = j;
                        while j < bytes.len() && bytes[j] != q {
                            j += 1;
                        }
                        if j < bytes.len() {
                            let raw = &open_tag[start..j];
                            let s = raw.trim();
                            if s.is_empty() || s.starts_with('-') || s.starts_with('+') {
                                return None;
                            }
                            match s.parse::<u64>() {
                                Ok(0) => return None,
                                Ok(n) => return Some(n),
                                Err(_) => return None,
                            }
                        }
                        return None;
                    }
                    return None;
                }
            }
        }
        i += 1;
    }
    None
}

/// Scan one document (or file-block) slice for `<segment>` opens in order,
/// returning one slot per element.
fn segment_bytes_in(slice: &str) -> Vec<Option<u64>> {
    let mut out = Vec::new();
    let mut rest = slice;
    while let Some(i) = rest.find("<segment") {
        let after = rest.as_bytes().get(i + "<segment".len()).copied();
        if after.is_some_and(is_tag_name_char) {
            // `<segments>` container or a longer name: not a segment.
            rest = &rest[i + "<segment".len()..];
            continue;
        }
        let tag_rest = &rest[i..];
        let Some(end) = tag_rest.find('>') else {
            break;
        };
        out.push(bytes_attr(&tag_rest[..end]));
        rest = &tag_rest[end + 1..];
    }
    out
}

/// Split the document into `<file>…</file>` blocks in NZB order.
fn file_blocks(doc: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut pos = 0;
    while pos < doc.len() {
        let Some(rel) = doc[pos..].find("<file") else {
            break;
        };
        let abs = pos + rel;
        if doc
            .as_bytes()
            .get(abs + "<file".len())
            .copied()
            .is_some_and(is_tag_name_char)
        {
            pos = abs + "<file".len();
            continue;
        }
        let tail = &doc[abs..];
        if let Some(close_rel) = tail.find("</file") {
            let after_close = &tail[close_rel..];
            if let Some(gt) = after_close.find('>') {
                let end = abs + close_rel + gt + 1;
                blocks.push(&doc[abs..end]);
                pos = end;
                continue;
            }
        }
        // Unclosed trailing file: take the rest of the document.
        blocks.push(&doc[abs..]);
        break;
    }
    blocks
}

/// Per-segment byte ranges sourced from NZB `bytes=` metadata.
///
/// Returns `(flat_in_document_order, per_file_in_nzb_order)`. Every slot is
/// `Some(n)` for a usable positive range, `None` for explicit-unknown
/// (missing, non-numeric, zero, or negative `bytes=`), never `Some(0)`.
pub fn parse_segment_byte_ranges(nzb_xml: &str) -> (Vec<Option<u64>>, Vec<Vec<Option<u64>>>) {
    let flat = segment_bytes_in(nzb_xml);
    let per_file: Vec<Vec<Option<u64>>> = file_blocks(nzb_xml)
        .iter()
        .map(|block| segment_bytes_in(block))
        .collect();
    (flat, per_file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{SMALL_NZB, SMALL_NZB_SEGMENTS};
    use crate::nzb_intake::estimate_nzb;

    fn parsed_small() -> ParsedNzb {
        ParsedNzb {
            file_count: 1,
            segment_count: 2,
            message_ids: SMALL_NZB_SEGMENTS.iter().map(|s| s.to_string()).collect(),
            segment_bytes: vec![None, None],
            file_segment_bytes: vec![vec![None, None]],
        }
    }

    #[test]
    fn parse_matching_intake_passes_the_cross_check() {
        let est = estimate_nzb(SMALL_NZB).unwrap();
        assert!(check_parsed(&est, &parsed_small()).is_ok());
    }

    #[test]
    fn parse_disagreeing_with_intake_fails_before_probe() {
        let est = estimate_nzb(SMALL_NZB).unwrap();
        let mut wrong_counts = parsed_small();
        wrong_counts.segment_count = 99;
        assert!(check_parsed(&est, &wrong_counts).is_err());
        let mut wrong_ids = parsed_small();
        wrong_ids.message_ids.pop();
        assert!(check_parsed(&est, &wrong_ids).is_err());
        let mut bad_id = parsed_small();
        bad_id.message_ids[0].clear();
        assert!(check_parsed(&est, &bad_id).is_err());
    }

    #[test]
    fn parse_rejects_command_injection_ids() {
        let est = estimate_nzb(SMALL_NZB).unwrap();
        let mut evil = parsed_small();
        evil.message_ids[1] = "x@y>\r\nAUTHINFO USER attacker\r\nQUIT\r\n<z".to_string();
        assert!(check_parsed(&est, &evil).is_err());
        let mut space = parsed_small();
        space.message_ids[0] = "has space@synthetic.invalid".to_string();
        assert!(check_parsed(&est, &space).is_err());
    }

    #[test]
    fn message_ids_chunk_into_probe_batches() {
        let batches = probe_batches(&parsed_small());
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 2);
        let empty = ParsedNzb {
            file_count: 0,
            segment_count: 0,
            message_ids: vec![],
            segment_bytes: vec![],
            file_segment_bytes: vec![],
        };
        assert!(probe_batches(&empty).is_empty());
    }

    #[test]
    fn large_parses_respect_the_probe_cap() {
        let ids: Vec<String> = (0..(crate::probe::MAX_PROBE_SEGMENTS + 1))
            .map(|i| format!("seg-{i}@synthetic.invalid"))
            .collect();
        let parsed = ParsedNzb {
            file_count: 1,
            segment_count: ids.len(),
            message_ids: ids.clone(),
            segment_bytes: vec![None; ids.len()],
            file_segment_bytes: vec![vec![None; ids.len()]],
        };
        let batches = probe_batches(&parsed);
        assert_eq!(batches.len(), 2);
        assert!(batches
            .iter()
            .all(|b| b.len() <= crate::probe::MAX_PROBE_SEGMENTS));
    }
}
