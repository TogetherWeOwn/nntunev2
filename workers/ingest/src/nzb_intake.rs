//! NZB upload intake guard.
//!
//! Uploads are untrusted user input: cap the bytes, require a well-formed
//! NZB-looking document, and bound files/segments/declared-bytes before
//! anything downstream (pool probes, par2 verify) spends provider connections
//! on them. Full NZB parsing (subject decoding, segment maps) lives in
//! `nntune-core` (core card); this module is the cheap pre-filter at the
//! worker edge.
//!
//! ## Versioned caps
//!
//! All caps live in [`IntakeLimits`] under [`INTAKE_CONFIG_VERSION`]. The
//! route validates with [`DEFAULT_LIMITS`]; tests pin the version and assert
//! that tightening a copy rejects bytes the default accepts, so a cap change
//! is a version bump, never a silent hardcoded edit.
//!
//! ## 4xx taxonomy
//!
//! Every rejection maps to a stable `code` + HTTP status the client can
//! switch on; count/byte variants also carry the measured value and the cap
//! so the client can render a reason without parsing message text:
//!
//! | code | status | fields |
//! |------|--------|--------|
//! | `NZB_EMPTY` | 400 | `intake_config_version` |
//! | `NZB_TOO_LARGE` | 413 | `intake_config_version`, `size_bytes`, `max_bytes` |
//! | `NZB_NOT_NZB` | 400 | `intake_config_version` |
//! | `NZB_MALFORMED` | 400 | `intake_config_version` |
//! | `NZB_TOO_MANY_FILES` | 413 | `intake_config_version`, `file_count`, `max_files` |
//! | `NZB_TOO_MANY_SEGMENTS` | 413 | `intake_config_version`, `segment_count`, `max_segments` |
//! | `NZB_DECLARED_BYTES_OVER_CAP` | 413 | `intake_config_version`, `declared_bytes`, `max_declared_bytes` |
//!
//! Checks run in the order above: the first failure wins.

/// Intake config version. Bump whenever a default cap changes; refusals
/// include this value so clients and dashboards can identify the policy.
pub const INTAKE_CONFIG_VERSION: u32 = 1;

/// Largest accepted upload: NZBs are XML metadata, never content.
pub const MAX_NZB_BYTES: usize = 2 * 1024 * 1024;
/// Upper bound on `<segment>` elements in one NZB: reachable inside
/// [`MAX_NZB_BYTES`] (a 2 MiB NZB holds tens of thousands of segments at
/// realistic per-segment overhead), so this fires before the size cap does.
pub const MAX_SEGMENTS: usize = 60_000;
/// Upper bound on `<file` elements in one NZB.
pub const MAX_FILES: usize = 5_000;
/// Upper bound on the summed `bytes="…"` attributes of all `<segment>`
/// elements. 64 GiB is the most 60k segments can plausibly declare (~1 MiB
/// per segment plus headroom); anything above is an absurd claim that must
/// fail at intake, not after provider connections are spent. Matches the
/// prior Go ingest bound for the same signal.
pub const MAX_DECLARED_BYTES: u64 = 64 * 1024 * 1024 * 1024;

/// Versioned intake caps. `version` pins the policy that produced a decision;
/// [`DEFAULT_LIMITS`] is what the route enforces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntakeLimits {
    pub version: u32,
    pub max_nzb_bytes: usize,
    pub max_files: usize,
    pub max_segments: usize,
    pub max_declared_bytes: u64,
}

impl IntakeLimits {
    /// The policy the route enforces under [`INTAKE_CONFIG_VERSION`].
    pub const fn versioned_default() -> Self {
        Self {
            version: INTAKE_CONFIG_VERSION,
            max_nzb_bytes: MAX_NZB_BYTES,
            max_files: MAX_FILES,
            max_segments: MAX_SEGMENTS,
            max_declared_bytes: MAX_DECLARED_BYTES,
        }
    }
}

impl Default for IntakeLimits {
    fn default() -> Self {
        Self::versioned_default()
    }
}

/// The policy the route enforces. Refer to the caps through this (or a copy
/// of it), never through the bare `MAX_*` constants in new code.
pub const DEFAULT_LIMITS: IntakeLimits = IntakeLimits::versioned_default();

/// Why an upload was refused (surfaced as 4xx with a stable [`code`](IntakeRejection::code),
/// never logged with the body). Count/byte variants carry the measured value
/// and the cap so the client can render the reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntakeRejection {
    Empty,
    TooLarge { size_bytes: usize, max_bytes: usize },
    NotNzb,
    Malformed,
    TooManyFiles { count: usize, max: usize },
    TooManySegments { count: usize, max: usize },
    DeclaredBytesOverCap { declared: u64, max: u64 },
}

impl IntakeRejection {
    /// Stable machine-readable code for this refusal. Clients switch on this,
    /// never on the human `message`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Empty => "NZB_EMPTY",
            Self::TooLarge { .. } => "NZB_TOO_LARGE",
            Self::NotNzb => "NZB_NOT_NZB",
            Self::Malformed => "NZB_MALFORMED",
            Self::TooManyFiles { .. } => "NZB_TOO_MANY_FILES",
            Self::TooManySegments { .. } => "NZB_TOO_MANY_SEGMENTS",
            Self::DeclaredBytesOverCap { .. } => "NZB_DECLARED_BYTES_OVER_CAP",
        }
    }

    /// HTTP status for this refusal: 400 for "not an NZB", 413 when a cap bit.
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Empty | Self::NotNzb | Self::Malformed => 400,
            Self::TooLarge { .. }
            | Self::TooManyFiles { .. }
            | Self::TooManySegments { .. }
            | Self::DeclaredBytesOverCap { .. } => 413,
        }
    }

    /// Human-readable reason. The stable contract is [`code`](Self::code) +
    /// status; this text is for logs and default rendering only.
    pub fn message(&self) -> &'static str {
        match self {
            Self::Empty => "nzb_xml must not be empty",
            Self::TooLarge { .. } => "nzb_xml exceeds the size cap",
            Self::NotNzb => "nzb_xml is not an NZB document",
            Self::Malformed => "nzb_xml is not well-formed XML",
            Self::TooManyFiles { .. } => "nzb has too many files",
            Self::TooManySegments { .. } => "nzb has too many segments",
            Self::DeclaredBytesOverCap { .. } => "nzb declared bytes over cap",
        }
    }
}

/// Cheap structural estimate from bounded scans of the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntakeEstimate {
    pub file_count: usize,
    pub segment_count: usize,
    /// Summed `bytes="…"` over all `<segment>` elements (saturating).
    pub declared_bytes: u64,
}

/// True for bytes that can continue an XML element name: anything after
/// `<{name}` in this set means this is a longer name (`<segments>`,
/// `<segmentx>`), not the element we count.
fn is_name_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':')
}

/// True for the first byte of an XML element name.
fn is_name_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || matches!(b, b'_' | b':')
}

#[derive(Debug, Clone, Copy)]
struct OpenTag {
    start: usize,
    name_end: usize,
    end: usize,
}

fn find_bytes(bytes: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    bytes[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| from + offset)
}

/// Skip an XML declaration, including a DOCTYPE internal subset.
fn skip_declaration(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 2;
    let mut quote = None;
    let mut subset_depth = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(open) = quote {
            if b == open {
                quote = None;
            }
        } else if b == b'\'' || b == b'"' {
            quote = Some(b);
        } else if b == b'[' {
            subset_depth += 1;
        } else if b == b']' {
            subset_depth = subset_depth.saturating_sub(1);
        } else if b == b'>' && subset_depth == 0 {
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

/// Return the next opening XML tag, skipping comments, CDATA, processing
/// instructions, declarations, and closing tags. The returned end offset is
/// past `>` so subsequent scans never revisit the tag or its attributes.
fn next_open_tag(body: &str, mut i: usize) -> Option<OpenTag> {
    let bytes = body.as_bytes();
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        if bytes[i..].starts_with(b"<!--") {
            i = find_bytes(bytes, b"-->", i + 4)? + 3;
            continue;
        }
        if bytes[i..].starts_with(b"<![CDATA[") {
            i = find_bytes(bytes, b"]]>", i + 9)? + 3;
            continue;
        }
        if bytes[i..].starts_with(b"<?") {
            i = find_bytes(bytes, b"?>", i + 2)? + 2;
            continue;
        }
        if bytes[i..].starts_with(b"<!") {
            i = skip_declaration(bytes, i)?;
            continue;
        }
        if bytes[i..].starts_with(b"</") {
            i = find_bytes(bytes, b">", i + 2)? + 1;
            continue;
        }

        let start = i;
        let name_start = i + 1;
        let name_end = read_name(bytes, name_start)?;
        let mut j = name_end;
        let mut quote = None;
        while j < bytes.len() {
            let b = bytes[j];
            if let Some(open) = quote {
                if b == open {
                    quote = None;
                }
            } else if b == b'\'' || b == b'"' {
                quote = Some(b);
            } else if b == b'>' {
                break;
            }
            j += 1;
        }
        if j >= bytes.len() {
            return None;
        }
        return Some(OpenTag {
            start,
            name_end,
            end: j + 1,
        });
    }
    None
}

/// Count opening elements with local name `name`, stopping once `over` is
/// exceeded. Prefixes are ignored so this agrees with nntune-core's
/// namespace-aware local-name dispatch.
fn count_elements(body: &str, name: &str, over: usize) -> usize {
    let mut count = 0;
    let mut offset = 0;
    while let Some(tag) = next_open_tag(body, offset) {
        let qualified_name = &body[tag.start + 1..tag.name_end];
        if qualified_name.rsplit(':').next() == Some(name) {
            count += 1;
            if count > over {
                break;
            }
        }
        offset = tag.end;
    }
    count
}

/// Skip ASCII whitespace, returning the new offset.
fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Read an XML name starting at `i`. Returns the end offset, or `None` when
/// no valid name starts there.
fn read_name(bytes: &[u8], i: usize) -> Option<usize> {
    if i >= bytes.len() || !is_name_start(bytes[i]) {
        return None;
    }
    let mut j = i + 1;
    while j < bytes.len() && is_name_char(bytes[j]) {
        j += 1;
    }
    Some(j)
}

/// Find the closing quote for an attribute value: offset just past it, or
/// `None` when unterminated.
fn skip_quoted(bytes: &[u8], i: usize) -> Option<usize> {
    if i >= bytes.len() || (bytes[i] != b'"' && bytes[i] != b'\'') {
        return None;
    }
    let q = bytes[i];
    let mut j = i + 1;
    while j < bytes.len() && bytes[j] != q {
        j += 1;
    }
    if j >= bytes.len() {
        return None;
    }
    Some(j + 1)
}

/// Check that `body` is well-formed XML (balanced tags, quoted attributes).
///
/// This is a deliberately small structural check, not a validating parser:
/// comments, processing instructions, CDATA and `<!…>` declarations are
/// skipped; every other `<…>` must be a well-formed open, close, or
/// self-closing tag with balanced nesting. Anything else — a truncated tail,
/// a stray close, a mismatch, an unterminated quote — is malformed. Operates
/// on bytes and never slices a `str` at a non-boundary.
fn check_well_formed(body: &str) -> bool {
    let bytes = body.as_bytes();
    let mut i = 0;
    let mut stack: Vec<(usize, usize)> = Vec::new();
    // NZBs nest ~4 deep (nzb > file > segments > segment); anything past
    // this is pathological nesting, not content.
    const MAX_DEPTH: usize = 1024;
    let mut saw_element = false;

    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let rest = &body[i..];
        if rest.starts_with("<!--") {
            match body[i..].find("-->") {
                Some(k) => i += k + 3,
                None => return false,
            }
        } else if rest.starts_with("<?") {
            match body[i..].find("?>") {
                Some(k) => i += k + 2,
                None => return false,
            }
        } else if rest.starts_with("<![CDATA[") {
            match body[i..].find("]]>") {
                Some(k) => i += k + 3,
                None => return false,
            }
        } else if rest.starts_with("<!") {
            match skip_declaration(bytes, i) {
                Some(end) => i = end,
                None => return false,
            }
        } else if rest.starts_with("</") {
            let mut j = skip_ws(bytes, i + 2);
            let name_end = match read_name(bytes, j) {
                Some(e) => e,
                None => return false,
            };
            let name = (j, name_end);
            j = skip_ws(bytes, name_end);
            if j >= bytes.len() || bytes[j] != b'>' {
                return false;
            }
            match stack.pop() {
                Some(open) if body[open.0..open.1] == body[name.0..name.1] => {}
                _ => return false,
            }
            i = j + 1;
        } else {
            // Opening (or self-closing) tag.
            let name_end = match read_name(bytes, i + 1) {
                Some(e) => e,
                None => return false,
            };
            saw_element = true;
            let mut j = name_end;
            let mut self_closing = false;
            loop {
                j = skip_ws(bytes, j);
                if j >= bytes.len() {
                    return false;
                }
                if bytes[j] == b'>' {
                    j += 1;
                    break;
                }
                if bytes[j] == b'/' {
                    if bytes.get(j + 1) != Some(&b'>') {
                        return false;
                    }
                    j += 2;
                    self_closing = true;
                    break;
                }
                // Attribute name.
                let attr_end = match read_name(bytes, j) {
                    Some(e) => e,
                    None => return false,
                };
                j = skip_ws(bytes, attr_end);
                if j < bytes.len() && bytes[j] == b'=' {
                    j = skip_ws(bytes, j + 1);
                    match skip_quoted(bytes, j) {
                        Some(e) => j = e,
                        None => return false,
                    }
                }
                // Valueless attributes are tolerated (lenient); the `>`
                // / `/>` checks at the loop top keep the tag bounded.
            }
            if !self_closing {
                if stack.len() >= MAX_DEPTH {
                    return false;
                }
                stack.push((i + 1, name_end));
            }
            i = j;
        }
    }
    saw_element && stack.is_empty()
}

/// Sum the `bytes="…"` attributes of every `<segment…>` tag. The shared
/// tokenizer ignores tag-like text inside non-element XML constructs and
/// advances past each full tag, making the scan linear in document length.
fn sum_segment_bytes(body: &str) -> Result<u64, ()> {
    let mut total: u64 = 0;
    let mut offset = 0;
    while let Some(open) = next_open_tag(body, offset) {
        let qualified_name = &body[open.start + 1..open.name_end];
        if qualified_name.rsplit(':').next() != Some("segment") {
            offset = open.end;
            continue;
        }
        let tag = &body[open.start..open.end];
        let tag_bytes = tag.as_bytes();
        let mut k = open.name_end - open.start - 1;
        let mut tag_value: Option<u64> = None;
        loop {
            k = skip_ws(tag_bytes, k);
            if k >= tag_bytes.len() || tag_bytes[k] == b'>' || tag[k..].starts_with("/>") {
                break;
            }
            let Some(attr_end) = read_name(tag_bytes, k) else {
                break;
            };
            let attr_name = &tag[k..attr_end];
            k = skip_ws(tag_bytes, attr_end);
            if k < tag_bytes.len() && tag_bytes[k] == b'=' {
                k = skip_ws(tag_bytes, k + 1);
                if k >= tag_bytes.len() || (tag_bytes[k] != b'"' && tag_bytes[k] != b'\'') {
                    return Err(());
                }
                let quote = tag_bytes[k];
                let value_start = k + 1;
                let mut value_end = value_start;
                while value_end < tag_bytes.len() && tag_bytes[value_end] != quote {
                    value_end += 1;
                }
                if value_end >= tag_bytes.len() {
                    return Err(());
                }
                if attr_name == "bytes" && tag_value.is_none() {
                    let raw = &tag[value_start..value_end];
                    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
                        return Err(());
                    }
                    match raw.parse::<u64>() {
                        Ok(n) => tag_value = Some(n),
                        Err(_) => total = total.saturating_add(u64::MAX),
                    }
                }
                k = value_end + 1;
            }
        }
        if let Some(n) = tag_value {
            total = total.saturating_add(n);
        }
        offset = open.end;
    }
    Ok(total)
}

/// Validate an upload body against `limits` and estimate its shape.
pub fn estimate_nzb_with_limits(
    body: &str,
    limits: &IntakeLimits,
) -> Result<IntakeEstimate, IntakeRejection> {
    if body.is_empty() {
        return Err(IntakeRejection::Empty);
    }
    if body.len() > limits.max_nzb_bytes {
        return Err(IntakeRejection::TooLarge {
            size_bytes: body.len(),
            max_bytes: limits.max_nzb_bytes,
        });
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
    if !check_well_formed(body) {
        return Err(IntakeRejection::Malformed);
    }
    let file_count = count_elements(body, "file", limits.max_files);
    if file_count > limits.max_files {
        return Err(IntakeRejection::TooManyFiles {
            count: file_count,
            max: limits.max_files,
        });
    }
    let segment_count = count_elements(body, "segment", limits.max_segments);
    if segment_count > limits.max_segments {
        return Err(IntakeRejection::TooManySegments {
            count: segment_count,
            max: limits.max_segments,
        });
    }
    let declared_bytes = sum_segment_bytes(body).map_err(|_| IntakeRejection::Malformed)?;
    if declared_bytes > limits.max_declared_bytes {
        return Err(IntakeRejection::DeclaredBytesOverCap {
            declared: declared_bytes,
            max: limits.max_declared_bytes,
        });
    }
    Ok(IntakeEstimate {
        file_count,
        segment_count,
        declared_bytes,
    })
}

/// Validate an upload body and estimate its shape under [`DEFAULT_LIMITS`].
pub fn estimate_nzb(body: &str) -> Result<IntakeEstimate, IntakeRejection> {
    estimate_nzb_with_limits(body, &DEFAULT_LIMITS)
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

    /// Two segments declaring 40 GiB each: tiny XML, 80 GiB of claims.
    const OVER_BYTES_NZB: &str = r#"<?xml version="1.0"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
<file poster="a@b" date="1" subject="big.iso">
<groups><group>alt.binaries.music</group></groups>
<segments>
<segment bytes="42949672960" number="1">big-one@news</segment>
<segment bytes="42949672960" number="2">big-two@news</segment>
</segments>
</file>
</nzb>"#;

    const MALFORMED_NZB: &str = r#"<?xml version="1.0"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
<file poster="a@b" date="1" subject="track01.flac">
<groups><group>alt.binaries.music</group></groups>
<segments>
<segment bytes="100" number="1">seg-one@news</segment>
</segments>
</nzb>"#;

    #[test]
    fn accepts_small_nzb_with_counts_and_declared_bytes() {
        let est = estimate_nzb(SMALL_NZB).unwrap();
        assert_eq!(est.file_count, 1);
        assert_eq!(est.segment_count, 2);
        assert_eq!(est.declared_bytes, 200);
    }

    #[test]
    fn caps_live_in_versioned_config_not_bare_consts() {
        assert_eq!(INTAKE_CONFIG_VERSION, 1);
        assert_eq!(DEFAULT_LIMITS.version, INTAKE_CONFIG_VERSION);
        assert_eq!(DEFAULT_LIMITS.max_nzb_bytes, MAX_NZB_BYTES);
        assert_eq!(DEFAULT_LIMITS.max_files, MAX_FILES);
        assert_eq!(DEFAULT_LIMITS.max_segments, MAX_SEGMENTS);
        assert_eq!(DEFAULT_LIMITS.max_declared_bytes, MAX_DECLARED_BYTES);
        // Tighten exactly one cap per assertion so each configured field is
        // independently proven to affect the estimate.
        assert!(estimate_nzb(SMALL_NZB).is_ok());
        let tight = IntakeLimits {
            max_nzb_bytes: SMALL_NZB.len() - 1,
            ..DEFAULT_LIMITS
        };
        assert_eq!(
            estimate_nzb_with_limits(SMALL_NZB, &tight),
            Err(IntakeRejection::TooLarge {
                size_bytes: SMALL_NZB.len(),
                max_bytes: SMALL_NZB.len() - 1,
            })
        );
        let tight = IntakeLimits {
            max_files: 0,
            ..DEFAULT_LIMITS
        };
        assert_eq!(
            estimate_nzb_with_limits(SMALL_NZB, &tight),
            Err(IntakeRejection::TooManyFiles { count: 1, max: 0 })
        );
        let tight = IntakeLimits {
            max_segments: 1,
            ..DEFAULT_LIMITS
        };
        assert_eq!(
            estimate_nzb_with_limits(SMALL_NZB, &tight),
            Err(IntakeRejection::TooManySegments { count: 2, max: 1 })
        );
        let tight = IntakeLimits {
            max_declared_bytes: 199,
            ..DEFAULT_LIMITS
        };
        assert_eq!(
            estimate_nzb_with_limits(SMALL_NZB, &tight),
            Err(IntakeRejection::DeclaredBytesOverCap {
                declared: 200,
                max: 199,
            })
        );
    }

    #[test]
    fn intake_corpus_asserts_exact_codes() {
        // Valid queues.
        assert!(estimate_nzb(SMALL_NZB).is_ok());
        // Malformed XML is a 400 with a stable code.
        let malformed = estimate_nzb(MALFORMED_NZB).unwrap_err();
        assert_eq!(malformed, IntakeRejection::Malformed);
        assert_eq!(malformed.code(), "NZB_MALFORMED");
        assert_eq!(malformed.http_status(), 400);
        // Declared-bytes over cap is a 413 carrying both numbers.
        let over_bytes = estimate_nzb(OVER_BYTES_NZB).unwrap_err();
        assert_eq!(over_bytes.code(), "NZB_DECLARED_BYTES_OVER_CAP");
        assert_eq!(over_bytes.http_status(), 413);
        match over_bytes {
            IntakeRejection::DeclaredBytesOverCap { declared, max } => {
                assert_eq!(declared, 2 * 42_949_672_960);
                assert_eq!(max, MAX_DECLARED_BYTES);
            }
            other => panic!("expected declared-bytes rejection, got {other:?}"),
        }
    }

    #[test]
    fn every_rejection_has_a_stable_code_and_status() {
        let cases: &[(IntakeRejection, &str, u16)] = &[
            (IntakeRejection::Empty, "NZB_EMPTY", 400),
            (
                IntakeRejection::TooLarge {
                    size_bytes: 3,
                    max_bytes: 2,
                },
                "NZB_TOO_LARGE",
                413,
            ),
            (IntakeRejection::NotNzb, "NZB_NOT_NZB", 400),
            (IntakeRejection::Malformed, "NZB_MALFORMED", 400),
            (
                IntakeRejection::TooManyFiles { count: 6, max: 5 },
                "NZB_TOO_MANY_FILES",
                413,
            ),
            (
                IntakeRejection::TooManySegments { count: 61, max: 60 },
                "NZB_TOO_MANY_SEGMENTS",
                413,
            ),
            (
                IntakeRejection::DeclaredBytesOverCap {
                    declared: 11,
                    max: 10,
                },
                "NZB_DECLARED_BYTES_OVER_CAP",
                413,
            ),
        ];
        for (rejection, code, status) in cases {
            assert_eq!(rejection.code(), *code);
            assert_eq!(rejection.http_status(), *status);
        }
    }

    #[test]
    fn rejects_empty_oversize_and_non_nzb_with_codes() {
        assert_eq!(estimate_nzb("").unwrap_err().code(), "NZB_EMPTY");
        let too_large = estimate_nzb(&"x".repeat(MAX_NZB_BYTES + 1)).unwrap_err();
        assert_eq!(too_large.code(), "NZB_TOO_LARGE");
        assert_eq!(too_large.http_status(), 413);
        match too_large {
            IntakeRejection::TooLarge {
                size_bytes,
                max_bytes,
            } => {
                assert_eq!(size_bytes, MAX_NZB_BYTES + 1);
                assert_eq!(max_bytes, MAX_NZB_BYTES);
            }
            other => panic!("expected too-large, got {other:?}"),
        }
        assert_eq!(
            estimate_nzb("<html><body>hi</body></html>").unwrap_err(),
            IntakeRejection::NotNzb
        );
        assert_eq!(
            estimate_nzb(r#"{"json": "not xml"}"#).unwrap_err(),
            IntakeRejection::NotNzb
        );
    }

    #[test]
    fn malformed_shapes_all_fail_well_formedness() {
        // Unclosed file element.
        assert_eq!(
            estimate_nzb(MALFORMED_NZB).unwrap_err(),
            IntakeRejection::Malformed
        );
        // Mismatched close.
        assert_eq!(
            estimate_nzb("<?xml?><nzb><file></files></nzb>").unwrap_err(),
            IntakeRejection::Malformed
        );
        // Stray close with nothing open.
        assert_eq!(
            estimate_nzb("<?xml?><nzb></segment></nzb>").unwrap_err(),
            IntakeRejection::Malformed
        );
        // Unterminated attribute quote.
        assert_eq!(
            estimate_nzb("<?xml?><nzb><file poster=\"abc></file></nzb>").unwrap_err(),
            IntakeRejection::Malformed
        );
        // Truncated tail.
        assert_eq!(
            estimate_nzb("<?xml?><nzb><file>").unwrap_err(),
            IntakeRejection::Malformed
        );
        // Non-numeric segment bytes are a lie, not a size.
        assert_eq!(
            estimate_nzb("<?xml?><nzb><segment bytes=\"lots\">x@y</segment></nzb>").unwrap_err(),
            IntakeRejection::Malformed
        );
    }

    #[test]
    fn unterminated_doctype_subset_is_malformed_before_counting_elements() {
        // The `<nzb>` root keeps the NotNzb pre-filter from firing first, so
        // this pins the well-formedness gate: an unterminated internal subset
        // must be `Malformed`, never an element count that hides the decoys.
        let mut doc = String::from("<?xml?><!DOCTYPE nzb [><nzb>");
        for _ in 0..(MAX_SEGMENTS + 1) {
            doc.push_str("<segment>");
        }

        assert_eq!(estimate_nzb(&doc), Err(IntakeRejection::Malformed));
    }

    #[test]
    fn well_formedness_accepts_legal_xml_shapes() {
        for doc in [
            "<?xml?><nzb><segment>x@y</segment></nzb>",
            "<?xml version=\"1.0\"?><nzb><!-- a comment --><segment>x@y</segment></nzb>",
            "<?xml?><nzb><segment/> </nzb>",
            "<?xml?><!DOCTYPE nzb><nzb><segment>x@y</segment></nzb>",
            "<?xml?><nzb><![CDATA[<not a tag>]]><segment>x@y</segment></nzb>",
            "<?xml?><nzb><file subject=\"a>b\"><segment bytes='100'>x@y</segment></file></nzb>",
        ] {
            // Zero-file/zero-segment docs still pass intake (the dispatch
            // layer refuses to enqueue jobs with nothing to probe); the
            // point here is none of these legal shapes is malformed.
            assert!(
                estimate_nzb(doc).is_ok(),
                "{doc:?} is well-formed and must pass intake"
            );
        }
    }

    #[test]
    fn declared_bytes_overflow_saturates_into_the_cap() {
        // A single claim that overflows u64 is over any sane cap, not a parse
        // error to quibble over.
        let doc = "<?xml?><nzb><segment bytes=\"99999999999999999999999\">x@y</segment></nzb>";
        match estimate_nzb(doc).unwrap_err() {
            IntakeRejection::DeclaredBytesOverCap { declared, max } => {
                assert!(declared > max);
            }
            other => panic!("expected declared-bytes rejection, got {other:?}"),
        }
    }

    #[test]
    fn segment_container_and_longer_names_do_not_feed_caps() {
        // `<segments>` holds no bytes and counts as neither a file nor a
        // segment, even with a bytes-looking attribute.
        let doc = "<?xml?><nzb><segments bytes=\"99999999999999\"><segment bytes=\"100\">x@y</segment></segments></nzb>";
        let est = estimate_nzb(doc).unwrap();
        assert_eq!(est.segment_count, 1);
        assert_eq!(est.declared_bytes, 100);
    }

    #[test]
    fn tag_like_text_in_non_element_constructs_does_not_feed_caps() {
        let doc = concat!(
            "<?xml?><!DOCTYPE nzb [<!ENTITY sample \"<segment bytes='90000000000'>\">]>\n",
            "<nzb><!-- <file><segment bytes=\"99999999999\"> -->",
            "<![CDATA[<file><segment bytes=\"88888888888\">]]>",
            "<?target <file><segment bytes=\"77777777777\">?>",
            "<file><segment bytes=\"100\">x@y</segment></file></nzb>"
        );
        let estimate = estimate_nzb(doc).unwrap();
        assert_eq!(estimate.file_count, 1);
        assert_eq!(estimate.segment_count, 1);
        assert_eq!(estimate.declared_bytes, 100);
    }

    #[test]
    fn many_comment_decoys_are_skipped_without_rescanning_tags() {
        let mut doc = String::from("<?xml?><nzb><!--");
        for _ in 0..5_000 {
            doc.push_str("<segment bytes=\"99999999999\">");
            doc.push_str(&"x".repeat(100));
        }
        doc.push_str("--><segment bytes=\"42\">x@y</segment></nzb>");
        assert!(doc.len() < MAX_NZB_BYTES);
        let estimate = estimate_nzb(&doc).unwrap();
        assert_eq!(estimate.segment_count, 1);
        assert_eq!(estimate.declared_bytes, 42);
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
        for sep in [" ", "\t", "\r", "\n", ">"] {
            let doc = format!("<?xml?><nzb><segment{sep}></segment></nzb>");
            let est = estimate_nzb(&doc).unwrap();
            assert_eq!(est.segment_count, 1, "separator {sep:?} must count");
        }
        // …including the self-closing form…
        let est = estimate_nzb("<?xml?><nzb><segment/></nzb>").unwrap();
        assert_eq!(est.segment_count, 1);
        // …while the container and longer names do not.
        let doc = "<?xml?><nzb><segments><segment >x</segment></segments></nzb>";
        assert_eq!(estimate_nzb(doc).unwrap().segment_count, 1);
        let doc = "<?xml?><nzb><segmentx>y</segmentx></nzb>";
        assert_eq!(estimate_nzb(doc).unwrap().segment_count, 0);
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
        let rejected = estimate_nzb(&bomb).unwrap_err();
        assert_eq!(rejected.code(), "NZB_TOO_MANY_SEGMENTS");
        assert_eq!(rejected.http_status(), 413);
        match rejected {
            IntakeRejection::TooManySegments { count, max } => {
                assert!(count > max);
                assert_eq!(max, MAX_SEGMENTS);
            }
            other => panic!("expected segment-cap rejection, got {other:?}"),
        }
    }

    #[test]
    fn rejects_prefixed_segment_declared_bytes_over_cap() {
        let mut doc = String::from("<?xml?><nzb><segment bytes=\"1\">x@y</segment>");
        for _ in 0..40 {
            doc.push_str("<x:segment bytes=\"2000000000\">x@y</x:segment>");
        }
        doc.push_str("</nzb>");

        let rejected = estimate_nzb(&doc).unwrap_err();
        assert_eq!(rejected.code(), "NZB_DECLARED_BYTES_OVER_CAP");
        assert_eq!(rejected.http_status(), 413);
        match rejected {
            IntakeRejection::DeclaredBytesOverCap { declared, max } => {
                assert_eq!(declared, 80_000_000_001);
                assert_eq!(max, MAX_DECLARED_BYTES);
            }
            other => panic!("expected declared-bytes rejection, got {other:?}"),
        }
    }

    #[test]
    fn rejects_file_bomb_with_taxonomy_fields() {
        let mut bomb = String::from("<?xml?><nzb>");
        for _ in 0..(MAX_FILES + 1) {
            bomb.push_str("<file></file>");
        }
        bomb.push_str("</nzb>");
        assert!(bomb.len() < MAX_NZB_BYTES);

        let rejected = estimate_nzb(&bomb).unwrap_err();
        assert_eq!(rejected.code(), "NZB_TOO_MANY_FILES");
        assert_eq!(rejected.http_status(), 413);
        match rejected {
            IntakeRejection::TooManyFiles { count, max } => {
                assert_eq!(count, MAX_FILES + 1);
                assert_eq!(max, MAX_FILES);
            }
            other => panic!("expected file-cap rejection, got {other:?}"),
        }
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
        let rejected = estimate_nzb(&bomb).unwrap_err();
        assert_eq!(rejected.code(), "NZB_TOO_MANY_SEGMENTS");
        assert_eq!(rejected.http_status(), 413);
    }
}
