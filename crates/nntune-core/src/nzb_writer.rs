//! Inert input types for future NZB writing, with redacted diagnostics.
//!
//! This module does not yet validate inputs or emit NZB documents.

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
