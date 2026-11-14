//! Inert target identity parsing and normalization.

/// Maximum target identifier length, in bytes.
pub const MAX_TARGET_ID_LEN: usize = 512;

/// A supported target class, parsed case-sensitively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Track,
    Release,
}

impl TargetKind {
    pub fn parse(s: &str) -> Option<TargetKind> {
        match s {
            "track" => Some(TargetKind::Track),
            "release" => Some(TargetKind::Release),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            TargetKind::Track => "track",
            TargetKind::Release => "release",
        }
    }
}

/// Checks the common identifier shape, not kind-specific requirements.
pub fn valid_target_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_TARGET_ID_LEN
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Lowercases catalogue MBIDs while preserving opaque identifiers.
pub fn canonical_target_id(value: &str) -> String {
    if crate::catalogue::is_catalogue_mbid(value) {
        value.to_ascii_lowercase()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_kind_parsing_is_exact_and_roundtrips() {
        for kind in [TargetKind::Track, TargetKind::Release] {
            assert_eq!(TargetKind::parse(kind.as_str()), Some(kind));
        }
        for value in ["", "Track", "TRACK", "Release", "release ", " track"] {
            assert_eq!(TargetKind::parse(value), None, "{value:?}");
        }
    }

    #[test]
    fn target_validation_and_normalization_preserve_opaque_ids() {
        let mbid = "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA";
        assert_eq!(canonical_target_id(mbid), mbid.to_ascii_lowercase());
        assert_eq!(canonical_target_id("Job_A-7"), "Job_A-7");
        assert!(valid_target_id("Job_A-7"));
        assert!(valid_target_id(
            "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-"
        ));
        assert!(valid_target_id(&"a".repeat(512)));
        for value in [
            "",
            "a/b",
            "a:b",
            "a.b",
            "a b",
            "a\0b",
            "a\nb",
            "é",
            &"a".repeat(513),
        ] {
            assert!(!valid_target_id(value), "{value:?}");
        }
    }
}
