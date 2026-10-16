//! Parse glue: project bounded parser output into probe envelopes.
//!
//! NZB parsing is delegated to `nntune-core`; this module owns the ingest
//! projection, the cross-check that keeps a parse disagreeing with the intake
//! guard from spending provider connections, and batching into probe envelopes.

use serde::{Deserialize, Serialize};

use crate::nzb_intake::IntakeEstimate;
use crate::probe::{validate_message_id, MAX_PROBE_SEGMENTS};

/// Ingest-side projection of a parsed NZB. The parser's richer metadata is
/// discarded here; only validated segment ids cross the provider boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedNzb {
    pub file_count: usize,
    pub segment_count: usize,
    pub message_ids: Vec<String>,
}

/// Why an uploaded NZB failed the shared-parser validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseRejection {
    Invalid(&'static str),
    TooLarge,
}

/// Parse uploaded NZB bytes with the shared bounded parser, then verify its
/// counts and identifiers against the conservative intake scan.
pub fn parse_upload(xml: &[u8], estimate: &IntakeEstimate) -> Result<ParsedNzb, ParseRejection> {
    let document = nntune_core::nzb::parse(xml).map_err(|error| match error {
        nntune_core::Error::Bounds { .. } => ParseRejection::TooLarge,
        _ => ParseRejection::Invalid("nntune-core rejected NZB"),
    })?;
    let file_count = document.files.len();
    let message_ids = document
        .files
        .into_iter()
        .flat_map(|file| file.segments.into_iter().map(|segment| segment.message_id))
        .collect::<Vec<_>>();
    let parsed = ParsedNzb {
        file_count,
        segment_count: message_ids.len(),
        message_ids,
    };
    check_parsed(estimate, &parsed).map_err(ParseRejection::Invalid)?;
    Ok(parsed)
}

/// Cross-check parser counts and identifiers against the intake scan. The
/// cheap scan skips comments, CDATA and attribute text; the `<=` bound only
/// tolerates interpretation differences with the parser, which may count
/// fewer elements but must never count more.
pub fn check_parsed(estimate: &IntakeEstimate, parsed: &ParsedNzb) -> Result<(), &'static str> {
    if parsed.file_count > estimate.file_count || parsed.segment_count > estimate.segment_count {
        return Err("parse exceeds the intake estimate");
    }
    if parsed.message_ids.len() != parsed.segment_count {
        return Err("message-id count must match the segment count");
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
        }
    }

    #[test]
    fn upload_parse_uses_core_document_and_matches_intake() {
        let est = estimate_nzb(SMALL_NZB).unwrap();
        let parsed = parse_upload(SMALL_NZB.as_bytes(), &est).unwrap();
        assert_eq!(parsed, parsed_small());
    }

    #[test]
    fn tag_like_comment_text_does_not_fail_the_parser_cross_check() {
        let xml = SMALL_NZB.replace(
            "</nzb>",
            "<!-- retry <segment bytes=\"1\" number=\"9\">note</segment> -->\n</nzb>",
        );
        let est = estimate_nzb(&xml).unwrap();
        assert_eq!(est.segment_count, 2);
        assert_eq!(parse_upload(xml.as_bytes(), &est).unwrap(), parsed_small());
    }

    #[test]
    fn shared_parser_bounds_are_reported_as_too_large() {
        let mut xml = String::from(
            "<nzb><file poster=\"p\" date=\"0\" subject=\"x\"><groups><group>x</group></groups><segments>",
        );
        for number in 1..=4097 {
            xml.push_str(&format!(
                "<segment bytes=\"1\" number=\"{number}\">s{number}@example.invalid</segment>"
            ));
        }
        xml.push_str("</segments></file></nzb>");
        let est = estimate_nzb(&xml).unwrap();
        assert_eq!(
            parse_upload(xml.as_bytes(), &est),
            Err(ParseRejection::TooLarge)
        );
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
            message_ids: ids,
        };
        let batches = probe_batches(&parsed);
        assert_eq!(batches.len(), 2);
        assert!(batches
            .iter()
            .all(|b| b.len() <= crate::probe::MAX_PROBE_SEGMENTS));
    }
}
