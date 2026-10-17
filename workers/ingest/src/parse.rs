//! Parse glue: the parsed shape the checkpoint stores.
//!
//! Full NZB parsing (subject decoding, yEnc segment maps) is owned by the
//! nntune-core card; this module owns the ingest side: the [`ParsedNzb`]
//! shape the `Parsing` step must produce, the cross-check that keeps a parse
//! disagreeing with the intake guard from spending provider connections, and
//! the batching into probe-sized envelopes.

use serde::{Deserialize, Serialize};

use crate::nzb_intake::IntakeEstimate;
use crate::probe::{validate_message_id, MAX_PROBE_SEGMENTS, MAX_SEGMENT_BYTES};

/// Match the parser's compiled-in cap for total declared NZB bytes.
const MAX_ENCODED_BYTES: u64 = 64 << 30;

/// What the `Parsing` step stores in the checkpoint: the segment map the
/// pool probe walks. Produced by `nntune-core` once that card lands; until
/// then tests and the synthetic E2E construct it from fixtures.
///
/// `segment_bytes` runs flat in document order, parallel to `message_ids`;
/// `file_segment_bytes` runs per-file in NZB order. Values are the NZB's
/// declared encoded wire sizes, not decoded file offsets; `None` is
/// explicit-unknown (the NZB carried no usable `bytes=` for that segment).
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
    if parsed.file_segment_bytes.len() != parsed.file_count {
        return Err("per-file byte-range group count must match the file count");
    }
    let ranged: usize = parsed.file_segment_bytes.iter().map(|f| f.len()).sum();
    if ranged != parsed.segment_count {
        return Err("per-file byte-range count must match the segment count");
    }
    // The flat and per-file lists are two views of the same ranges: they
    // must agree slot for slot in NZB order, not just in total count, or
    // the stream path would range-seek the wrong segments.
    if !parsed
        .file_segment_bytes
        .iter()
        .flatten()
        .eq(parsed.segment_bytes.iter())
    {
        return Err("per-file byte ranges must match the flat segment byte ranges in order");
    }
    // Missing metadata is explicit-unknown (None). Keep known ranges within
    // the core parser's per-segment and document-wide encoded-byte limits.
    let total_bytes = parsed.segment_bytes.iter().try_fold(0u64, |total, bytes| {
        match bytes {
            Some(n) if *n == 0 || *n > MAX_SEGMENT_BYTES => None,
            Some(n) => total.checked_add(*n),
            None => Some(total),
        }
    });
    let Some(total_bytes) = total_bytes else {
        return Err("segment byte ranges exceed their valid bounds");
    };
    if total_bytes > MAX_ENCODED_BYTES {
        return Err("total segment byte ranges exceed the document limit");
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
    assert_eq!(
        parsed.message_ids.len(),
        parsed.segment_bytes.len(),
        "message ids and segment byte ranges must be parallel"
    );
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
        // A `bytes` lookalike inside another attribute's quoted value is
        // not an attribute name: skip quoted spans while scanning.
        if bytes[i] == b'"' || bytes[i] == b'\'' {
            let quote = bytes[i];
            i += 1;
            while i < bytes.len() && bytes[i] != quote {
                i += 1;
            }
            i += 1;
            continue;
        }
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
                                Ok(n) if (1..=MAX_SEGMENT_BYTES).contains(&n) => return Some(n),
                                _ => return None,
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

/// Index of the `>` that ends an opening tag, ignoring any `>` inside a
/// quoted attribute value (valid XML such as `note="a>b"`).
fn opening_tag_end(tag: &str) -> Option<usize> {
    let bytes = tag.as_bytes();
    let mut quote: Option<u8> = None;
    for (i, &b) in bytes.iter().enumerate() {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => return Some(i),
            None => {}
        }
    }
    None
}

/// Scan a document (or file-block) slice for unprefixed or namespaced
/// `<segment>` opens, ignoring comments and other markup.
fn segment_bytes_in(slice: &str) -> Vec<Option<u64>> {
    let mut out = Vec::new();
    let mut rest = slice;
    while let Some(i) = rest.find('<') {
        rest = &rest[i..];
        if rest.starts_with("<!--") {
            let Some(end) = rest.find("-->") else {
                break;
            };
            rest = &rest[end + 3..];
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            let Some(end) = rest.find("]]>") else {
                break;
            };
            rest = &rest[end + 3..];
            continue;
        }
        let Some(end) = opening_tag_end(rest) else {
            break;
        };
        let tag = &rest[..end];
        let name = tag
            .strip_prefix('<')
            .unwrap_or_default()
            .split(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>')
            .next()
            .unwrap_or_default();
        if !name.starts_with('/') && name.rsplit(':').next() == Some("segment") {
            out.push(bytes_attr(tag));
        }
        rest = &rest[end + 1..];
    }
    out
}

/// Split actual `<file>…</file>` elements into NZB-order blocks.
fn file_blocks(doc: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut rest = doc;
    let mut offset = 0;
    let mut file_start = None;
    while let Some(i) = rest.find('<') {
        offset += i;
        rest = &rest[i..];
        if rest.starts_with("<!--") {
            let Some(end) = rest.find("-->") else {
                break;
            };
            let next = end + 3;
            offset += next;
            rest = &rest[next..];
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            let Some(end) = rest.find("]]>") else {
                break;
            };
            let next = end + 3;
            offset += next;
            rest = &rest[next..];
            continue;
        }
        let Some(end) = opening_tag_end(rest) else {
            break;
        };
        let tag = &rest[..=end];
        let raw_name = tag.strip_prefix('<').unwrap_or_default();
        let closing = raw_name.starts_with('/');
        let name = raw_name
            .trim_start_matches('/')
            .split(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>')
            .next()
            .unwrap_or_default();
        let local_name = name.rsplit(':').next();
        let self_closing = tag[..end].trim_end().ends_with('/');
        if local_name == Some("file") {
            if closing {
                if let Some(start) = file_start.take() {
                    blocks.push(&doc[start..offset + end + 1]);
                }
            } else if self_closing {
                blocks.push(&doc[offset..offset + end + 1]);
            } else {
                file_start = Some(offset);
            }
        }
        let next = end + 1;
        offset += next;
        rest = &rest[next..];
    }
    if let Some(start) = file_start {
        // An unclosed trailing file: preserve the remaining block for the
        // later parser to reject or cross-check.
        blocks.push(&doc[start..]);
    }
    blocks
}

/// Per-segment encoded wire sizes sourced from NZB `bytes=` metadata.
///
/// These are size hints, not decoded file offsets. Returns
/// `(flat_in_document_order, per_file_in_nzb_order)`. Every slot is `Some(n)`
/// for a usable positive size, `None` for explicit-unknown (missing,
/// non-numeric, zero, negative, or out-of-bounds `bytes=`).
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
    fn parse_rejects_range_representations_that_disagree() {
        let est = estimate_nzb(SMALL_NZB).unwrap();
        // Same counts, but the per-file ranges assign different sizes to
        // the segments than the flat list: the two representations must
        // agree in order, not just in total length.
        let mut reversed = parsed_small();
        reversed.segment_bytes = vec![Some(100), Some(200)];
        reversed.file_segment_bytes = vec![vec![Some(200), Some(100)]];
        assert!(check_parsed(&est, &reversed).is_err());
        // Flattened values agree, but the number of per-file groups must
        // match the file count.
        let mut wrong_groups = parsed_small();
        wrong_groups.segment_bytes = vec![Some(100), Some(200)];
        wrong_groups.file_segment_bytes = vec![vec![Some(100)], vec![Some(200)]];
        assert!(check_parsed(&est, &wrong_groups).is_err());
    }

    #[test]
    fn segment_ranges_parse_past_angle_brackets_in_quoted_attributes() {
        // A `>` inside a quoted attribute value does not end the opening
        // tag, and a `bytes=` lookalike inside a quoted value is not the
        // segment's range.
        let doc = r#"<nzb><file><segments>
<segment note="a>b" bytes="100" number="1">seg-a1@synthetic.invalid</segment>
<segment note='x>y' bytes='200' number="2">seg-a2@synthetic.invalid</segment>
<segment note="bytes=999" bytes="300" number="3">seg-a3@synthetic.invalid</segment>
</segments></file></nzb>"#;
        let (flat, per_file) = parse_segment_byte_ranges(doc);
        assert_eq!(flat, vec![Some(100), Some(200), Some(300)]);
        assert_eq!(per_file, vec![vec![Some(100), Some(200), Some(300)]]);
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
