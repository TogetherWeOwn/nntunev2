//! NZB upload intake guard.
//!
//! Uploads are untrusted user input: cap the bytes, require an NZB-looking
//! document, and bound files/segments before anything downstream (pool
//! probes, par2 verify) spends provider connections on them. Full NZB
//! parsing (subject decoding, segment maps) lives in `nntune-core`
//! (core card); this module is the cheap pre-filter at the worker edge.

/// Largest accepted upload: NZBs are XML metadata, never content.
pub const MAX_NZB_BYTES: usize = 2 * 1024 * 1024;
/// Upper bound on `<segment>` elements in one NZB: reachable inside
/// [`MAX_NZB_BYTES`] (a 2 MiB NZB holds tens of thousands of segments at
/// realistic per-segment overhead), so this fires before the size cap does.
pub const MAX_SEGMENTS: usize = 60_000;
/// Upper bound on `<file` elements in one NZB.
pub const MAX_FILES: usize = 5_000;

/// Why an upload was refused (surfaced as 400/413, never logged with body).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntakeRejection {
    Empty,
    TooLarge,
    NotNzb,
    TooManyFiles,
    TooManySegments,
}

/// Cheap structural estimate from one bounded scan of the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntakeEstimate {
    pub file_count: usize,
    pub segment_count: usize,
}

/// Count `needle` occurrences, stopping once `over` is exceeded.
fn count_capped(haystack: &str, needle: &str, over: usize) -> usize {
    let mut n = 0;
    let mut rest = haystack;
    while let Some(i) = rest.find(needle) {
        n += 1;
        if n > over {
            break;
        }
        rest = &rest[i + needle.len()..];
    }
    n
}

/// Count segment elements without matching the `<segments>` container:
/// a `<segment` prefix only counts when followed by a space or `>`.
fn count_segments(haystack: &str, over: usize) -> usize {
    let mut n = 0;
    let mut rest = haystack;
    while let Some(i) = rest.find("<segment") {
        let after = rest.as_bytes().get(i + "<segment".len()).copied();
        if matches!(after, Some(b' ') | Some(b'>')) {
            n += 1;
            if n > over {
                break;
            }
        }
        rest = &rest[i + "<segment".len()..];
    }
    n
}

/// Validate an upload body and estimate its shape.
pub fn estimate_nzb(body: &str) -> Result<IntakeEstimate, IntakeRejection> {
    if body.is_empty() {
        return Err(IntakeRejection::Empty);
    }
    if body.len() > MAX_NZB_BYTES {
        return Err(IntakeRejection::TooLarge);
    }
    let head = &body[..body.len().min(1024)];
    let looks_like_nzb = head.contains("<nzb") || (head.contains("<?xml") && body.contains("<nzb"));
    if !looks_like_nzb {
        return Err(IntakeRejection::NotNzb);
    }
    let file_count = count_capped(body, "<file", MAX_FILES);
    if file_count > MAX_FILES {
        return Err(IntakeRejection::TooManyFiles);
    }
    let segment_count = count_segments(body, MAX_SEGMENTS);
    if segment_count > MAX_SEGMENTS {
        return Err(IntakeRejection::TooManySegments);
    }
    Ok(IntakeEstimate {
        file_count,
        segment_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMALL_NZB: &str = r#"<?xml version="1.0"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
<file poster="a@b" date="1" subject="track01.flac">
<groups><group>alt.binaries.music</group></groups>
<segments>
<segment bytes="100" number="1">seg-one@news</segment>
<segment bytes="100" number="2">seg-two@news</segment>
</segments>
</file>
</nzb>"#;

    #[test]
    fn accepts_small_nzb_with_counts() {
        let est = estimate_nzb(SMALL_NZB).unwrap();
        assert_eq!(est.file_count, 1);
        assert_eq!(est.segment_count, 2);
    }

    #[test]
    fn rejects_empty_oversize_and_non_nzb() {
        assert_eq!(estimate_nzb(""), Err(IntakeRejection::Empty));
        assert_eq!(
            estimate_nzb(&"x".repeat(MAX_NZB_BYTES + 1)),
            Err(IntakeRejection::TooLarge)
        );
        assert_eq!(
            estimate_nzb("<html><body>hi</body></html>"),
            Err(IntakeRejection::NotNzb)
        );
        assert_eq!(
            estimate_nzb(r#"{"json": "not xml"}"#),
            Err(IntakeRejection::NotNzb)
        );
    }

    #[test]
    fn rejects_segment_bomb() {
        // Compact segments stay under MAX_NZB_BYTES so the segment cap
        // (not the size cap) is what fires.
        let mut bomb = String::from("<?xml?><nzb>");
        for i in 0..(MAX_SEGMENTS + 10) {
            bomb.push_str(&format!("<segment>{i}</segment>"));
        }
        bomb.push_str("</nzb>");
        assert!(bomb.len() < MAX_NZB_BYTES);
        assert_eq!(estimate_nzb(&bomb), Err(IntakeRejection::TooManySegments));
    }
}
