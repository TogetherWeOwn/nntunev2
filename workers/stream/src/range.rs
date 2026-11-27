//! HTTP `Range` handling for the play path (seeking + `206 Partial Content`).
//!
//! Only single `bytes=` ranges are honoured; anything else (missing header,
//! multiple ranges, unparseable units) falls back to `200 OK` full-body per
//! RFC 9110 §14.2. An unsatisfiable range resolves to `416`.

use nntune_core::digest_geometry::{DigestGeometry, RangeGeometry};

/// A resolved inclusive byte range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    /// Inclusive end.
    pub end: u64,
}

impl ByteRange {
    pub fn len(&self) -> u64 {
        self.end - self.start + 1
    }

    pub fn is_empty(&self) -> bool {
        // Ranges are inclusive with `end >= start` by construction, so this
        // is always false; provided for the `len`/`is_empty` convention.
        self.end < self.start
    }
}

/// How to answer a play request for a body of `len` bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeOutcome {
    /// No (usable) Range header: serve `200 OK` with the full body.
    Full,
    /// Serve `206 Partial Content` with this slice.
    Partial(ByteRange),
    /// Serve `416 Range Not Satisfiable`.
    Unsatisfiable,
}

/// Parse an all-digit numeral: nonempty ASCII digits only. Bare
/// `str::parse::<u64>()` would also accept a leading `+`, and trimming
/// would wash whitespace, so the digit check comes first. Saturating is
/// sufficient for comparing an offset with any supported body length.
fn parse_digits(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(s.parse().unwrap_or(u64::MAX))
}

/// Parse `header` (the value of the `Range` header, if any) for a body of
/// `len` bytes.
pub fn parse(header: Option<&str>, len: u64) -> RangeOutcome {
    let spec = match header {
        Some(h) => match h.trim_matches([' ', '\t']).strip_prefix("bytes=") {
            // The outer trim is HTTP outer-whitespace only; the numerals
            // themselves are taken verbatim so interior whitespace (or a
            // leading `+`) falls back to `Full` below.
            Some(s) => s,
            None => return RangeOutcome::Full,
        },
        None => return RangeOutcome::Full,
    };
    if spec.contains(',') {
        // Multi-range: not supported, serve the full body.
        return RangeOutcome::Full;
    }
    let (start_s, end_s) = match spec.split_once('-') {
        Some(pair) => pair,
        None => return RangeOutcome::Full,
    };
    if start_s.is_empty() {
        // Suffix range: the last N bytes.
        let n: u64 = match parse_digits(end_s) {
            Some(n) => n,
            None => return RangeOutcome::Full,
        };
        if len == 0 || n == 0 {
            // A zero-length suffix (`bytes=-0`) requests no bytes, so it is
            // unsatisfiable on any body, not a full-body delivery.
            return RangeOutcome::Unsatisfiable;
        }
        let n = n.min(len);
        RangeOutcome::Partial(ByteRange {
            start: len - n,
            end: len - 1,
        })
    } else {
        let start: u64 = match parse_digits(start_s) {
            Some(s) => s,
            None => return RangeOutcome::Full,
        };
        if end_s.is_empty() {
            if start >= len {
                return RangeOutcome::Unsatisfiable;
            }
            RangeOutcome::Partial(ByteRange {
                start,
                end: len - 1,
            })
        } else {
            let end: u64 = match parse_digits(end_s) {
                Some(e) => e,
                None => return RangeOutcome::Full,
            };
            // Saturation can make distinct out-of-domain offsets equal.
            // Compare their significant decimal digits to detect inversion.
            let start_digits = start_s.trim_start_matches('0');
            let end_digits = end_s.trim_start_matches('0');
            if (start_digits.len(), start_digits) > (end_digits.len(), end_digits) {
                return RangeOutcome::Full;
            }
            if start >= len {
                return RangeOutcome::Unsatisfiable;
            }
            RangeOutcome::Partial(ByteRange {
                start,
                end: end.min(len - 1),
            })
        }
    }
}

/// Plan a request against the trusted plaintext length of an enrolled object.
/// No route calls this planner. It does not fetch, authenticate, or release bytes.
pub fn plan_encrypted_range(
    header: Option<&str>,
    geometry: DigestGeometry,
) -> (RangeOutcome, Option<RangeGeometry>) {
    let len = geometry.plaintext_len();
    let outcome = parse(header, len);
    let plan = match outcome {
        RangeOutcome::Full => geometry.range(0, len),
        RangeOutcome::Partial(range) => range
            .end
            .checked_add(1)
            .and_then(|end| geometry.range(range.start, end)),
        RangeOutcome::Unsatisfiable => None,
    };
    (outcome, plan)
}

/// Value for the `Content-Range` header of a `206` response.
pub fn content_range_value(range: ByteRange, len: u64) -> String {
    format!("bytes {}-{}/{}", range.start, range.end, len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_planning_uses_plaintext_range_semantics() {
        let geometry = DigestGeometry::new(65_537).unwrap();
        for (header, start, end, ciphertext) in [
            ("bytes=0-0", 0, 0, 48..65_600),
            ("bytes=65536-65536", 65_536, 65_536, 65_600..65_617),
            ("bytes=65535-", 65_535, 65_536, 48..65_617),
            ("bytes=-2", 65_535, 65_536, 48..65_617),
            ("bytes=-99999", 0, 65_536, 48..65_617),
            (
                "bytes=65536-18446744073709551615",
                65_536,
                65_536,
                65_600..65_617,
            ),
        ] {
            let (outcome, plan) = plan_encrypted_range(Some(header), geometry);
            assert_eq!(outcome, RangeOutcome::Partial(ByteRange { start, end }));
            assert_eq!(plan.unwrap().ciphertext_span(), Some(ciphertext));
        }
        for header in ["bytes=65537-", "bytes=65537-65538", "bytes=-0"] {
            assert_eq!(
                plan_encrypted_range(Some(header), geometry),
                (RangeOutcome::Unsatisfiable, None)
            );
        }
        let empty = DigestGeometry::new(0).unwrap();
        let (outcome, plan) = plan_encrypted_range(None, empty);
        assert_eq!(outcome, RangeOutcome::Full);
        assert_eq!(plan.unwrap().ciphertext_span(), Some(48..64));
        assert_eq!(
            plan_encrypted_range(Some("bytes=-1"), empty),
            (RangeOutcome::Unsatisfiable, None)
        );
    }

    #[test]
    fn missing_or_garbage_header_serves_full_body() {
        assert_eq!(parse(None, 100), RangeOutcome::Full);
        assert_eq!(parse(Some(""), 100), RangeOutcome::Full);
        assert_eq!(parse(Some("items=0-10"), 100), RangeOutcome::Full);
        assert_eq!(parse(Some("bytes=abc-def"), 100), RangeOutcome::Full);
        assert_eq!(parse(Some("bytes=0-10,20-30"), 100), RangeOutcome::Full);
    }

    #[test]
    fn open_ended_range_runs_to_end_of_body() {
        assert_eq!(
            parse(Some("bytes=10-"), 100),
            RangeOutcome::Partial(ByteRange { start: 10, end: 99 })
        );
    }

    #[test]
    fn closed_range_is_clamped_to_body() {
        assert_eq!(
            parse(Some("bytes=0-49"), 100),
            RangeOutcome::Partial(ByteRange { start: 0, end: 49 })
        );
        assert_eq!(
            parse(Some("bytes=50-9999"), 100),
            RangeOutcome::Partial(ByteRange { start: 50, end: 99 })
        );
    }

    #[test]
    fn suffix_range_serves_the_tail() {
        assert_eq!(
            parse(Some("bytes=-20"), 100),
            RangeOutcome::Partial(ByteRange { start: 80, end: 99 })
        );
        // Suffix longer than the body: whole body.
        assert_eq!(
            parse(Some("bytes=-500"), 100),
            RangeOutcome::Partial(ByteRange { start: 0, end: 99 })
        );
    }

    #[test]
    fn start_past_end_of_body_is_unsatisfiable() {
        assert_eq!(parse(Some("bytes=100-"), 100), RangeOutcome::Unsatisfiable);
        assert_eq!(
            parse(Some("bytes=200-300"), 100),
            RangeOutcome::Unsatisfiable
        );
        assert_eq!(parse(Some("bytes=90-10"), 100), RangeOutcome::Full);
        // Closed range starting exactly at the length: still unsatisfiable,
        // never `Partial { start: 100, end: 99 }`.
        assert_eq!(
            parse(Some("bytes=100-150"), 100),
            RangeOutcome::Unsatisfiable
        );
        assert_eq!(
            parse(Some("bytes=99-150"), 100),
            RangeOutcome::Partial(ByteRange { start: 99, end: 99 })
        );
    }

    #[test]
    fn empty_body_has_no_satisfiable_range() {
        assert_eq!(parse(Some("bytes=0-"), 0), RangeOutcome::Unsatisfiable);
        assert_eq!(parse(Some("bytes=0-0"), 0), RangeOutcome::Unsatisfiable);
        assert_eq!(parse(None, 0), RangeOutcome::Full);
    }

    #[test]
    fn content_range_header_value() {
        let range = ByteRange { start: 10, end: 99 };
        assert_eq!(content_range_value(range, 100), "bytes 10-99/100");
    }

    #[test]
    fn non_digit_numerals_fall_back_to_full_body() {
        // `u64::from_str` accepts a leading `+`, and the old trims washed
        // surrounding whitespace: both must resolve to `Full`, never
        // `Partial` (and never panic).
        assert_eq!(parse(Some("bytes=+0-9"), 100), RangeOutcome::Full);
        assert_eq!(parse(Some("bytes=0-+9"), 100), RangeOutcome::Full);
        assert_eq!(parse(Some("bytes= 0-9"), 100), RangeOutcome::Full);
        assert_eq!(parse(Some("bytes=0 -9"), 100), RangeOutcome::Full);
        assert_eq!(parse(Some("bytes=0- 9"), 100), RangeOutcome::Full);
        assert_eq!(parse(Some("bytes=-+5"), 100), RangeOutcome::Full);
        assert_eq!(parse(Some("bytes=- 5"), 100), RangeOutcome::Full);
        assert_eq!(parse(Some("bytes= -5"), 100), RangeOutcome::Full);
        // Empty numerals were and stay `Full`.
        assert_eq!(parse(Some("bytes=-"), 100), RangeOutcome::Full);
        assert_eq!(parse(Some("bytes="), 100), RangeOutcome::Full);
    }

    #[test]
    fn digit_only_shapes_keep_partial_delivery() {
        assert_eq!(
            parse(Some("bytes=0-9"), 100),
            RangeOutcome::Partial(ByteRange { start: 0, end: 9 })
        );
        assert_eq!(
            parse(Some("bytes=007-009"), 100),
            RangeOutcome::Partial(ByteRange { start: 7, end: 9 })
        );
        assert_eq!(
            parse(Some("bytes=-5"), 100),
            RangeOutcome::Partial(ByteRange { start: 95, end: 99 })
        );
        // Outer header whitespace is still the HTTP layer's, not a numeral.
        assert_eq!(
            parse(Some("  bytes=0-9  "), 100),
            RangeOutcome::Partial(ByteRange { start: 0, end: 9 })
        );
    }

    #[test]
    fn overflowing_numerals_resolve_against_the_body_without_wrapping() {
        let whole = RangeOutcome::Partial(ByteRange {
            start: 0,
            end: u64::MAX - 1,
        });
        for header in [
            "bytes=-18446744073709551616",
            "bytes=0-18446744073709551616",
        ] {
            assert_eq!(parse(Some(header), u64::MAX), whole, "{header}");
            assert_eq!(
                parse(Some(header), 0),
                RangeOutcome::Unsatisfiable,
                "{header}"
            );
        }
        for header in [
            "bytes=18446744073709551616-",
            "bytes=18446744073709551616-18446744073709551617",
        ] {
            assert_eq!(
                parse(Some(header), u64::MAX),
                RangeOutcome::Unsatisfiable,
                "{header}"
            );
        }
        for header in [
            "bytes=18446744073709551617-18446744073709551616",
            "bytes=18446744073709551616-0,0-1",
        ] {
            assert_eq!(
                parse(Some(header), u64::MAX),
                RangeOutcome::Full,
                "{header}"
            );
        }
    }

    #[test]
    fn only_http_optional_whitespace_is_trimmed() {
        assert_eq!(
            parse(Some("\t bytes=-1 \t"), 10),
            RangeOutcome::Partial(ByteRange { start: 9, end: 9 })
        );
        for header in ["\nbytes=-1", "bytes=-1\r", "\u{a0}bytes=-1"] {
            assert_eq!(parse(Some(header), 10), RangeOutcome::Full);
        }
    }

    #[test]
    fn range_numerals_require_a_digit_predicate_pin() {
        const SRC: &str = include_str!("range.rs");
        // Only the production section: the needle literal below lives in
        // this test module, so searching the whole file would self-match.
        let prod = SRC.split("#[cfg(test)]").next().unwrap_or(SRC);
        assert!(
            prod.contains("is_ascii_digit"),
            "range.rs must gate numerals on an ASCII-digit predicate: bare \
             `str::parse::<u64>()` accepts a leading `+`, so a revert to it \
             would silently re-admit `bytes=+0-9` as partial"
        );
    }
}
