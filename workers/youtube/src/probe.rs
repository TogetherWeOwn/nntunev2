//! Byte-probe planner for the GO/NO-GO gate.
//!
//! Per extracted video the Day-1 measurement fetches, from Worker egress:
//! the first 256 KiB, one centered 256 KiB mid-file range (once the head
//! response reveals the total length), and the full file. This module only
//! computes the `Range` header values; the Worker entry does the I/O.

/// Head and mid probe window: 256 KiB.
pub const HEAD_BYTES: u64 = 256 * 1024;

/// `Range` header for the head probe: the first 256 KiB.
pub fn head_range() -> String {
    format!("bytes=0-{}", HEAD_BYTES - 1)
}

/// `Range` header for a 256 KiB window centered in a `total_bytes` file.
/// `None` when the file fits in one head window (mid == head then).
pub fn mid_range(total_bytes: u64) -> Option<String> {
    if total_bytes <= HEAD_BYTES {
        return None;
    }
    let start = total_bytes / 2 - HEAD_BYTES / 2;
    Some(format!("bytes={}-{}", start, start + HEAD_BYTES - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_window_is_256kib() {
        assert_eq!(HEAD_BYTES, 262_144);
        assert_eq!(head_range(), "bytes=0-262143");
    }

    #[test]
    fn mid_window_is_centered() {
        // 10 MiB file: center 5242880, window 5111808..=5373951.
        assert_eq!(
            mid_range(10 * 1024 * 1024).as_deref(),
            Some("bytes=5111808-5373951")
        );
    }

    #[test]
    fn mid_window_absent_for_small_files() {
        assert_eq!(mid_range(HEAD_BYTES), None);
        assert_eq!(mid_range(100), None);
    }
}
