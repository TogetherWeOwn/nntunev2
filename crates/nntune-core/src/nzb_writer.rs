//! NZB writer: the emit side of [`crate::nzb`].
//!
//! The poster builds one NZB per file from the article plan (true
//! per-segment byte sizes — the yEnc-encoded size of each posted article
//! body) and seals the resulting document inside the posted-file record's
//! encrypted manifest envelope. The NZB itself never touches disk, a URL,
//! or a log line: [`write_nzb`] returns bytes for the caller to seal
//! in memory, and [`write_nzb_to`] streams into any writer the caller
//! provides.
//!
//! Every input is validated against the same bounds the parser enforces
//! ([`crate::nzb`] limits, contiguous 1-based segment numbers, bare
//! message-ID shape), so writer output always parses. Validation rejects
//! over-long segments, non-contiguous numbering, and empty structural
//! fields before any XML is emitted.

use std::io;

use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};
use quick_xml::Writer;

use crate::nzb::{
    normalize_message_id, MAX_DOCUMENT_BYTES, MAX_ENCODED_BYTES, MAX_FILES, MAX_SEGMENTS_PER_FILE,
    MAX_SEGMENT_BYTES, MAX_SEGMENT_TEXT_BYTES,
};
use crate::{Error, Result};

/// One posted article as an NZB segment. `message_id` is the bare article
/// ID (no angle brackets); `encoded_bytes` is the exact yEnc-encoded body
/// size the poster will upload for this segment.
#[derive(Clone, PartialEq, Eq)]
pub struct SegmentInput {
    pub number: usize,
    pub message_id: String,
    pub encoded_bytes: i64,
}

impl std::fmt::Debug for SegmentInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SegmentInput")
            .field("number", &self.number)
            .field("message_id", &"[REDACTED]")
            .field("encoded_bytes", &self.encoded_bytes)
            .finish()
    }
}

/// One posted file as an NZB `<file>` element.
#[derive(Clone, PartialEq, Eq)]
pub struct FileInput {
    /// Obfuscated subject (the poster never uses real file names here).
    pub subject: String,
    /// Poster identity as it appears on the wire.
    pub poster: String,
    /// Unix-epoch posting time, as the parser requires (`date >= 0`).
    pub date: i64,
    /// Newsgroups the file was posted to (at least one).
    pub groups: Vec<String>,
    /// Article segments in number order (`number` must run `1..=n`).
    pub segments: Vec<SegmentInput>,
}

impl std::fmt::Debug for FileInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileInput")
            .field("subject", &"[REDACTED]")
            .field("poster", &"[REDACTED]")
            .field("group_count", &self.groups.len())
            .field("segment_count", &self.segments.len())
            .finish()
    }
}

/// Serializes `files` to an NZB 1.1 document.
pub fn write_nzb(files: &[FileInput]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    write_nzb_to(&mut out, files)?;
    Ok(out)
}

/// Serializes `files` as NZB 1.1 into `sink`.
pub fn write_nzb_to<W: io::Write>(sink: W, files: &[FileInput]) -> Result<()> {
    validate(files)?;
    let mut writer = Writer::new(sink);
    writer
        .write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
        .map_err(|_| Error::Malformed("write NZB declaration failed".into()))?;
    let mut root = BytesStart::new("nzb");
    root.push_attribute(("xmlns", "http://www.newzbin.com/DTD/nzb/nzb-1.1"));
    write_start(&mut writer, root)?;
    for file in files {
        let mut element = BytesStart::new("file");
        element.push_attribute(("poster", file.poster.as_str()));
        let date = file.date.to_string();
        element.push_attribute(("date", date.as_str()));
        element.push_attribute(("subject", file.subject.as_str()));
        write_start(&mut writer, element)?;
        write_start(&mut writer, BytesStart::new("groups"))?;
        for group in &file.groups {
            write_start(&mut writer, BytesStart::new("group"))?;
            write_text(&mut writer, group)?;
            write_end(&mut writer, "group")?;
        }
        write_end(&mut writer, "groups")?;
        write_start(&mut writer, BytesStart::new("segments"))?;
        for segment in &file.segments {
            let mut element = BytesStart::new("segment");
            let bytes = segment.encoded_bytes.to_string();
            element.push_attribute(("bytes", bytes.as_str()));
            let number = segment.number.to_string();
            element.push_attribute(("number", number.as_str()));
            write_start(&mut writer, element)?;
            write_text(&mut writer, &segment.message_id)?;
            write_end(&mut writer, "segment")?;
        }
        write_end(&mut writer, "segments")?;
        write_end(&mut writer, "file")?;
    }
    write_end(&mut writer, "nzb")?;
    Ok(())
}

fn write_start<W: io::Write>(writer: &mut Writer<W>, element: BytesStart<'_>) -> Result<()> {
    writer
        .write_event(Event::Start(element))
        .map_err(|_| Error::Malformed("write NZB element failed".into()))
}

fn write_end<W: io::Write>(writer: &mut Writer<W>, name: &str) -> Result<()> {
    writer
        .write_event(Event::End(BytesEnd::new(name)))
        .map_err(|_| Error::Malformed("write NZB element failed".into()))
}

fn write_text<W: io::Write>(writer: &mut Writer<W>, text: &str) -> Result<()> {
    writer
        .write_event(Event::Text(BytesText::new(text)))
        .map_err(|_| Error::Malformed("write NZB text failed".into()))
}

/// Mirrors the parser's bounds so emitted documents always parse back:
/// file/segment counts, per-segment byte range, contiguous 1-based
/// numbering, bare message-ID shape, the cumulative encoded-bytes
/// ceiling, per-text-node byte caps, and the total document byte limit.
fn validate(files: &[FileInput]) -> Result<()> {
    if files.is_empty() {
        return Err(Error::Malformed("NZB needs at least one file".into()));
    }
    if files.len() > MAX_FILES {
        return Err(Error::Bounds {
            bound: "NZB file count",
            limit: MAX_FILES as i64,
        });
    }
    let mut total_encoded: i64 = 0;
    // Upper bound on the serialized document, so the parser's document
    // byte limit holds sight unseen: every text field plus a fixed
    // allowance per element for tags, attributes and separators. Text
    // is counted at its worst-case XML-escaped width (6 bytes per char,
    // for `&quot;`) and the allowance over-counts the real XML, so
    // passing this estimate guarantees the emitted document passes; no
    // caller can smuggle a parse-back failure past validate().
    let mut document_bytes: u64 = 512;
    for file in files {
        if file.subject.trim().is_empty() {
            return Err(Error::Malformed("NZB file subject is empty".into()));
        }
        if file.subject.len() > MAX_SEGMENT_TEXT_BYTES {
            return Err(Error::Bounds {
                bound: "NZB file subject",
                limit: MAX_SEGMENT_TEXT_BYTES as i64,
            });
        }
        if file.poster.is_empty() {
            return Err(Error::Malformed("NZB file poster is empty".into()));
        }
        if file.poster.len() > MAX_SEGMENT_TEXT_BYTES {
            return Err(Error::Bounds {
                bound: "NZB file poster",
                limit: MAX_SEGMENT_TEXT_BYTES as i64,
            });
        }
        if file.date < 0 {
            return Err(Error::Malformed("NZB file date is negative".into()));
        }
        if file.groups.is_empty() {
            return Err(Error::Malformed("NZB file needs at least one group".into()));
        }
        for group in &file.groups {
            if group.is_empty() {
                return Err(Error::Malformed("NZB group name is empty".into()));
            }
            if group.len() > MAX_SEGMENT_TEXT_BYTES {
                return Err(Error::Bounds {
                    bound: "NZB group name",
                    limit: MAX_SEGMENT_TEXT_BYTES as i64,
                });
            }
        }
        document_bytes += (file.subject.len() + file.poster.len()) as u64 * 6
            + file
                .groups
                .iter()
                .map(|g| g.len() as u64 * 6 + 24)
                .sum::<u64>()
            + 256;
        if file.segments.is_empty() {
            return Err(Error::Malformed(
                "NZB file needs at least one segment".into(),
            ));
        }
        if file.segments.len() > MAX_SEGMENTS_PER_FILE {
            return Err(Error::Bounds {
                bound: "NZB segment count",
                limit: MAX_SEGMENTS_PER_FILE as i64,
            });
        }
        for (index, segment) in file.segments.iter().enumerate() {
            if segment.number != index + 1 {
                return Err(Error::Malformed(format!(
                    "NZB segment numbers must run 1..=n in order, want {}, have {}",
                    index + 1,
                    segment.number
                )));
            }
            if !(1..=MAX_SEGMENT_BYTES).contains(&segment.encoded_bytes) {
                return Err(Error::Malformed(format!(
                    "NZB segment encoded bytes out of range: {}",
                    segment.encoded_bytes
                )));
            }
            total_encoded = total_encoded
                .checked_add(segment.encoded_bytes)
                .filter(|total| *total <= MAX_ENCODED_BYTES)
                .ok_or(Error::Bounds {
                    bound: "NZB total encoded bytes",
                    limit: MAX_ENCODED_BYTES,
                })?;
            // Rejects `<…>`-wrapped IDs, whitespace/control bytes, and
            // over-long IDs — the same bare-ID rule the parser applies.
            normalize_message_id(&segment.message_id)?;
            document_bytes += segment.message_id.len() as u64 * 6 + 64;
        }
        if document_bytes > MAX_DOCUMENT_BYTES {
            return Err(Error::Bounds {
                bound: "NZB document bytes",
                limit: MAX_DOCUMENT_BYTES as i64,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nzb::parse;

    fn test_id(n: usize) -> String {
        // Synthetic bare IDs in the `postie` shape (random local part, one
        // `@`); never real article references.
        format!("seg{n}.7f3a9c@test.post.invalid")
    }

    fn two_segment_file() -> FileInput {
        FileInput {
            subject: "obfuscated file 042 [1/2] & <special> \"chars\"".into(),
            poster: "poster@test.post.invalid".into(),
            date: 1_800_000_000,
            groups: vec!["alt.binaries.test".into()],
            segments: vec![
                SegmentInput {
                    number: 1,
                    message_id: test_id(1),
                    encoded_bytes: 725_000,
                },
                SegmentInput {
                    number: 2,
                    message_id: test_id(2),
                    encoded_bytes: 18_412,
                },
            ],
        }
    }

    #[test]
    fn sink_diagnostics_never_escape_the_writer() {
        struct FailAfter(usize);
        impl io::Write for FailAfter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if self.0 == 0 {
                    return Err(io::Error::other("sensitive sink context"));
                }
                let count = self.0.min(bytes.len());
                self.0 -= count;
                Ok(count)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let files = [two_segment_file()];
        let len = write_nzb(&files).unwrap().len();
        for budget in 0..len {
            let error = write_nzb_to(FailAfter(budget), &files).unwrap_err();
            assert!(!format!("{error:?} {error}").contains("sensitive sink context"));
        }
    }

    #[test]
    fn debug_redacts_nzb_custody() {
        let file = two_segment_file();
        let summary = format!("{file:?}");
        assert!(summary.contains("[REDACTED]"));
        assert!(summary.contains("group_count: 1"));
        assert!(summary.contains("segment_count: 2"));
        assert!(!summary.contains(&file.subject));
        assert!(!summary.contains(&file.poster));
        for group in &file.groups {
            assert!(!summary.contains(group));
        }
        for segment in &file.segments {
            assert!(!summary.contains(&segment.message_id));
            let summary = format!("{segment:?}");
            assert!(summary.contains("[REDACTED]"));
            assert!(!summary.contains(&segment.message_id));
        }
    }

    #[test]
    fn written_document_parses_with_exact_sizes() {
        let files = vec![two_segment_file()];
        let xml = write_nzb(&files).unwrap();
        let doc = parse(&xml).unwrap();
        assert_eq!(doc.files.len(), 1);
        let file = &doc.files[0];
        assert_eq!(file.subject, files[0].subject);
        assert_eq!(file.posted_at_unix, files[0].date);
        assert_eq!(doc.encoded_bytes, 725_000 + 18_412);
        assert_eq!(file.segments.len(), 2);
        for (segment, want) in file.segments.iter().zip(&files[0].segments) {
            assert_eq!(segment.number, want.number);
            assert_eq!(segment.message_id, want.message_id);
            assert_eq!(segment.encoded_bytes, want.encoded_bytes);
        }
    }

    #[test]
    fn subject_and_group_text_is_escaped() {
        let mut file = two_segment_file();
        file.groups = vec!["alt.binaries.&<>\"test".into()];
        let xml = write_nzb(&[file.clone()]).unwrap();
        // Raw control characters must not appear unescaped in the document.
        assert!(!xml.windows(9).any(|w| w == b"&<>\"test"));
        let doc = parse(&xml).unwrap();
        assert_eq!(doc.files[0].subject, file.subject);
        assert_eq!(doc.files[0].posted_at_unix, file.date);
    }

    #[test]
    fn validation_mirrors_parser_text_and_document_caps() {
        let good = two_segment_file();
        // Subject, poster and group text longer than the parser's
        // per-text-node cap must be rejected before any XML is emitted.
        let mut bad = good.clone();
        bad.subject = "s".repeat(MAX_SEGMENT_TEXT_BYTES + 1);
        assert!(write_nzb(&[bad]).is_err());
        let mut bad = good.clone();
        bad.poster = "p".repeat(MAX_SEGMENT_TEXT_BYTES + 1);
        assert!(write_nzb(&[bad]).is_err());
        let mut bad = good.clone();
        bad.groups = vec!["g".repeat(MAX_SEGMENT_TEXT_BYTES + 1)];
        assert!(write_nzb(&[bad]).is_err());
        // Files whose combined escaped text overruns the parser's
        // document byte limit must be rejected too. 64 files stays
        // under the accepted file count, so only the document cap can
        // fire here.
        let mut big = good.clone();
        big.subject = "s".repeat(100_000);
        let files: Vec<_> = (0..64).map(|_| big.clone()).collect();
        let result = write_nzb(&files);
        assert!(matches!(
            result,
            Err(crate::Error::Bounds {
                bound: "NZB document bytes",
                ..
            })
        ));
    }

    #[test]
    fn validation_rejects_bad_inputs() {
        let good = two_segment_file();
        // Empty file list.
        assert!(write_nzb(&[]).is_err());
        // Empty subject.
        let mut bad = good.clone();
        bad.subject = "  ".into();
        assert!(write_nzb(&[bad]).is_err());
        // Negative date.
        let mut bad = good.clone();
        bad.date = -1;
        assert!(write_nzb(&[bad]).is_err());
        // No groups.
        let mut bad = good.clone();
        bad.groups.clear();
        assert!(write_nzb(&[bad]).is_err());
        // Non-contiguous segment numbers.
        let mut bad = good.clone();
        bad.segments[1].number = 3;
        assert!(write_nzb(&[bad]).is_err());
        // Zero encoded bytes.
        let mut bad = good.clone();
        bad.segments[0].encoded_bytes = 0;
        assert!(write_nzb(&[bad]).is_err());
        // Bracketed message-ID.
        let mut bad = good.clone();
        bad.segments[0].message_id = format!("<{}>", test_id(1));
        assert!(write_nzb(&[bad]).is_err());
    }
}
