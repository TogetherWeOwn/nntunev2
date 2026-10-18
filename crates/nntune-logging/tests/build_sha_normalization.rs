//! Build-SHA normalization contract pin.
//!
//! Contract under test (`crates/nntune-logging/src/lib.rs:194,201-221`
//! `BUILD_SHA_UNKNOWN`, `parse_build_sha`, `is_build_sha_known`):
//!
//! - a 40-char lowercase hex commit SHA passes through byte-identical and
//!   reports known;
//! - anything else — absent (`None`), empty, truncated (39-char), over-long
//!   (41-char), uppercase, non-hex — normalizes to exactly `"unknown"` and
//!   reports not-known;
//! - `"unknown"` itself is not known (it is the absence marker, not an
//!   identity);
//! - invalid input is never echoed back: the output is either the valid
//!   input unchanged or the `"unknown"` sentinel, so a bad candidate can
//!   never leak into a log line as a fake identity.
//!
//! Sensitivity: fails under a length-relaxation mutant (e.g. `>= 40` instead
//! of `== 40` accepts the 41-char case), a case-folding mutant (accepting
//! uppercase hex), a non-hex-accepting mutant, a `None`-passthrough mutant,
//! or an echo-back mutant returning the raw candidate on invalid input.
//! The `unknown`-is-not-known assertion fails under a sentinel-accepting
//! mutant (`sha == BUILD_SHA_UNKNOWN => true`).
//!
//! Implementation is untouched by this pin; this file exercises only the
//! public `parse_build_sha` / `is_build_sha_known` surface.

use nntune_logging::{is_build_sha_known, parse_build_sha, BUILD_SHA_UNKNOWN};

const VALID_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const VALID_SHA_ALL_F: &str = "ffffffffffffffffffffffffffffffffffffffff";

/// 39-char truncation of [`VALID_SHA`] (one char short).
const SHA_39: &str = "0123456789abcdef0123456789abcdef0123456";
/// 41-char extension of [`VALID_SHA`] (one char over).
const SHA_41: &str = "0123456789abcdef0123456789abcdef012345670";
const SHA_UPPER: &str = "0123456789ABCDEF0123456789ABCDEF01234567";
const SHA_NON_HEX: &str = "0123456789abcdef0123456789abcdef0123456g";

#[test]
fn valid_40_char_lowercase_hex_round_trips() {
    for sha in [VALID_SHA, VALID_SHA_ALL_F] {
        assert_eq!(sha.len(), 40, "fixture must be 40 chars: {sha:?}");
        assert!(is_build_sha_known(sha), "valid SHA must be known: {sha:?}");
        assert_eq!(parse_build_sha(Some(sha)), sha);
        assert!(is_build_sha_known(&parse_build_sha(Some(sha))));
    }
}

#[test]
fn absent_empty_and_malformed_normalize_to_unknown() {
    // (candidate, label) pairs; `None` is the absent-candidate shape.
    let cases: &[(Option<&str>, &str)] = &[
        (None, "absent"),
        (Some(""), "empty"),
        (Some(SHA_39), "39-char truncated"),
        (Some(SHA_41), "41-char over-long"),
        (Some(SHA_UPPER), "uppercase hex"),
        (Some(SHA_NON_HEX), "non-hex"),
    ];
    for (raw, label) in cases {
        let normalized = parse_build_sha(*raw);
        assert_eq!(
            normalized, BUILD_SHA_UNKNOWN,
            "{label} candidate must normalize to unknown, got {normalized:?}"
        );
        assert!(
            !is_build_sha_known(&normalized),
            "{label} normalization must not be known"
        );
    }
}

#[test]
fn unknown_sentinel_itself_is_not_known() {
    assert_eq!(BUILD_SHA_UNKNOWN, "unknown");
    assert!(!is_build_sha_known(BUILD_SHA_UNKNOWN));
    assert_eq!(parse_build_sha(Some(BUILD_SHA_UNKNOWN)), BUILD_SHA_UNKNOWN);
}

#[test]
fn invalid_input_is_never_echoed_back() {
    // Every invalid non-sentinel candidate must map to the sentinel, never
    // back to itself — the raw value is dropped, not trusted.
    let invalid: &[&str] = &[
        "",
        SHA_39,
        SHA_41,
        SHA_UPPER,
        SHA_NON_HEX,
        "0123456789abcdef", // 16-char truncated prefix
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", // 64-hex digest
        "Bearer abc123",
        " 0123456789abcdef0123456789abcdef01234567",
        "0123456789abcdef0123456789abcdef01234567 ",
    ];
    for raw in invalid {
        let normalized = parse_build_sha(Some(*raw));
        assert_eq!(
            normalized, BUILD_SHA_UNKNOWN,
            "invalid input must normalize to unknown: {raw:?}"
        );
        assert_ne!(
            normalized, *raw,
            "invalid input must never be echoed back: {raw:?}"
        );
        assert!(
            !is_build_sha_known(raw),
            "invalid input must not be known: {raw:?}"
        );
    }
}
