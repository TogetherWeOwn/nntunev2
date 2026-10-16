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

/// True for bytes that can continue an XML element name.
fn is_name_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':')
}

fn starts_with_at(bytes: &[u8], at: usize, marker: &[u8]) -> bool {
    bytes.get(at..at.saturating_add(marker.len())) == Some(marker)
}

/// Skip an XML declaration, respecting quotes and an internal subset.
fn skip_declaration(bytes: &[u8], mut at: usize) -> usize {
    let mut quote = None;
    let mut subset_depth = 0usize;
    while at < bytes.len() {
        let byte = bytes[at];
        if let Some(q) = quote {
            if byte == q {
                quote = None;
            }
        } else {
            match byte {
                b'\'' | b'"' => quote = Some(byte),
                b'[' => subset_depth += 1,
                b']' => subset_depth = subset_depth.saturating_sub(1),
                b'>' if subset_depth == 0 => return at + 1,
                _ => {}
            }
        }
        at += 1;
    }
    at
}

/// Skip an element tag's attributes, so tag-like text in attribute values is
/// never counted as a nested element.
fn skip_tag(bytes: &[u8], mut at: usize) -> usize {
    let mut quote = None;
    while at < bytes.len() {
        let byte = bytes[at];
        if let Some(q) = quote {
            if byte == q {
                quote = None;
            }
        } else {
            match byte {
                b'\'' | b'"' => quote = Some(byte),
                b'>' => return at + 1,
                _ => {}
            }
        }
        at += 1;
    }
    at
}

/// Count actual XML start tags with the requested local name, stopping once
/// `over` is exceeded. Comments, CDATA, processing instructions, declarations,
/// closing tags, and tag-like text in attributes are skipped.
fn count_elements(haystack: &str, name: &str, over: usize) -> usize {
    let bytes = haystack.as_bytes();
    let mut at = 0;
    let mut count = 0;
    while at < bytes.len() {
        if bytes[at] != b'<' {
            at += 1;
            continue;
        }
        if starts_with_at(bytes, at, b"<!--") {
            at = bytes[at + 4..]
                .windows(3)
                .position(|w| w == b"-->")
                .map_or(bytes.len(), |i| at + 4 + i + 3);
            continue;
        }
        if starts_with_at(bytes, at, b"<![CDATA[") {
            at = bytes[at + 9..]
                .windows(3)
                .position(|w| w == b"]]>")
                .map_or(bytes.len(), |i| at + 9 + i + 3);
            continue;
        }
        if starts_with_at(bytes, at, b"<?") {
            at = bytes[at + 2..]
                .windows(2)
                .position(|w| w == b"?>")
                .map_or(bytes.len(), |i| at + 2 + i + 2);
            continue;
        }
        if starts_with_at(bytes, at, b"<!") {
            at = skip_declaration(bytes, at + 2);
            continue;
        }

        let mut name_start = at + 1;
        let closing = bytes.get(name_start) == Some(&b'/');
        if closing {
            name_start += 1;
        }
        let mut name_end = name_start;
        while name_end < bytes.len() && is_name_char(bytes[name_end]) {
            name_end += 1;
        }
        if name_end == name_start {
            at += 1;
            continue;
        }
        if !closing {
            let qname = &bytes[name_start..name_end];
            let local_name = qname.rsplit(|b| *b == b':').next().unwrap_or(qname);
            if local_name == name.as_bytes() {
                count += 1;
                if count > over {
                    return count;
                }
            }
        }
        at = skip_tag(bytes, name_end);
    }
    count
}

/// Validate an upload body and estimate its shape.
pub fn estimate_nzb(body: &str) -> Result<IntakeEstimate, IntakeRejection> {
    if body.is_empty() {
        return Err(IntakeRejection::Empty);
    }
    if body.len() > MAX_NZB_BYTES {
        return Err(IntakeRejection::TooLarge);
    }
    // Work on bytes, never on a str slice: a multibyte char straddling byte
    // 1024 would make `&body[..1024]` panic and abort the isolate on wasm.
    let head = &body.as_bytes()[..body.len().min(1024)];
    let head_has_nzb = head.windows(4).any(|w| w == b"<nzb");
    let head_has_xml = head.windows(5).any(|w| w == b"<?xml");
    // The full-body check needs no slicing and is therefore panic-free.
    if !(head_has_nzb || (head_has_xml && body.contains("<nzb"))) {
        return Err(IntakeRejection::NotNzb);
    }
    let file_count = count_elements(body, "file", MAX_FILES);
    if file_count > MAX_FILES {
        return Err(IntakeRejection::TooManyFiles);
    }
    let segment_count = count_elements(body, "segment", MAX_SEGMENTS);
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
    fn multibyte_char_at_head_boundary_does_not_panic() {
        // '€' is 3 bytes (1023..1026): a str slice at byte 1024 would panic.
        let mut body = String::from("<nzb>");
        body.push_str(&"a".repeat(1018));
        body.push('€');
        body.push_str("</nzb>");
        let est = estimate_nzb(&body).unwrap();
        assert_eq!(est.segment_count, 0);
    }

    #[test]
    fn segment_delimiter_variants_all_count() {
        // Every legal byte after the element name counts…
        for sep in [" ", "\t", "\r", "\n", ">", "/"] {
            let doc = format!("<?xml?><nzb><segment{sep}></segment></nzb>");
            let est = estimate_nzb(&doc).unwrap();
            assert_eq!(est.segment_count, 1, "separator {sep:?} must count");
        }
        // …while the container and longer names do not.
        let doc = "<?xml?><nzb><segments><segment >x</segment></segments></nzb>";
        assert_eq!(estimate_nzb(doc).unwrap().segment_count, 1);
        let doc = "<?xml?><nzb><segmentx>y</segmentx></nzb>";
        assert_eq!(estimate_nzb(doc).unwrap().segment_count, 0);
    }

    #[test]
    fn ignores_tag_like_text_in_comments_cdata_and_attributes() {
        let xml = r#"<?xml?><nzb note="<segment>"><!-- <segment> --><![CDATA[<segment>]]><segment>x</segment></nzb>"#;
        assert_eq!(estimate_nzb(xml).unwrap().segment_count, 1);
    }

    #[test]
    fn comment_tag_text_cannot_trigger_segment_cap() {
        let mut xml = String::from("<?xml?><nzb><!--");
        for _ in 0..=MAX_SEGMENTS {
            xml.push_str("<segment>");
        }
        xml.push_str("--></nzb>");
        assert!(xml.len() < MAX_NZB_BYTES);
        assert_eq!(estimate_nzb(&xml).unwrap().segment_count, 0);
    }

    #[test]
    fn newline_segment_bomb_hits_the_segment_cap() {
        // 70_010 newline-separated segments exceed the 60k cap in ~1.5 MB,
        // so the segment cap (not the size cap) must fire.
        let mut bomb = String::from("<?xml?><nzb>");
        for i in 0..(MAX_SEGMENTS + 10) {
            bomb.push_str(&format!("<segment\n>{i}</segment>"));
        }
        bomb.push_str("</nzb>");
        assert!(bomb.len() < MAX_NZB_BYTES);
        assert_eq!(estimate_nzb(&bomb), Err(IntakeRejection::TooManySegments));
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
