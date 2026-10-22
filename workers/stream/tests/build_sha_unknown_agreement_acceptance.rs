//! BUILD_SHA_UNKNOWN literal agreement pin (tests-only).
//!
//! Contract under test (`workers/stream/src/trace.rs:81,87-106`
//! `BUILD_SHA_UNKNOWN`, `is_build_sha_known`, `normalize_build_sha`,
//! documented mirror of `crates/nntune-logging/src/lib.rs:194,201-216`):
//!
//! - the stream sentinel is exactly the `"unknown"` literal;
//! - stream and logging sentinels agree byte-for-byte;
//! - a 40-char lowercase hex commit SHA passes through both normalizers
//!   unchanged and reports known on both sides;
//! - anything else — absent (`None`), empty, truncated, uppercase, non-hex,
//!   over-long (41-char), or a 64-hex digest — normalizes to the agreed
//!   sentinel on both sides and reports not-known;
//! - the sentinel itself is not known on either side, and invalid input is
//!   never echoed back.
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - changing the stream literal (e.g. `"UNKNOWN"`, `"n/a"`) fails
//!   `stream_sentinel_is_the_unknown_literal` and
//!   `sentinels_agree_byte_for_byte`;
//! - changing the logging literal fails `sentinels_agree_byte_for_byte` — the
//!   logging-side `"unknown"` pin lives in
//!   `crates/nntune-logging/tests/build_sha_normalization.rs` and only guards
//!   its own crate, so a one-sided drift dies here;
//! - a length-relaxation mutant on either side (e.g. `>= 40` instead of
//!   `== 40`) fails `garbage_uppercase_and_overlong_map_to_agreed_sentinel`
//!   on the 41-char case;
//! - a case-folding mutant (accepting uppercase hex) or a non-hex-accepting
//!   mutant fails the same test on the uppercase / non-hex cases;
//! - a sentinel-accepting mutant (`sha == BUILD_SHA_UNKNOWN => true`) fails
//!   the not-known assertions in `sentinel_itself_is_not_known_on_both_sides`;
//! - an echo-back mutant (returning the raw candidate on invalid input) fails
//!   `invalid_input_is_never_echoed_back`.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test -p nntune-stream --test build_sha_unknown_agreement_acceptance
//! ```

use nntune_logging::{
    is_build_sha_known as logging_is_known, parse_build_sha as logging_normalize,
    BUILD_SHA_UNKNOWN as LOGGING_UNKNOWN,
};
use nntune_stream::trace::{
    is_build_sha_known as stream_is_known, normalize_build_sha as stream_normalize,
    BUILD_SHA_UNKNOWN as STREAM_UNKNOWN,
};

const VALID_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const VALID_SHA_ALL_F: &str = "ffffffffffffffffffffffffffffffffffffffff";
/// 41-char extension of [`VALID_SHA`] (one char over).
const SHA_41: &str = "0123456789abcdef0123456789abcdef012345670";
/// 64-hex digest: valid hex, never a commit identity.
const SHA_64: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const SHA_UPPER: &str = "0123456789ABCDEF0123456789ABCDEF01234567";
const SHA_NON_HEX: &str = "0123456789abcdef0123456789abcdef0123456g";

#[test]
fn stream_sentinel_is_the_unknown_literal() {
    assert_eq!(
        STREAM_UNKNOWN, "unknown",
        "stream BUILD_SHA_UNKNOWN must stay exactly \"unknown\""
    );
    assert_eq!(
        STREAM_UNKNOWN.as_bytes(),
        b"unknown",
        "stream sentinel must agree byte-for-byte, not just on chars"
    );
}

#[test]
fn sentinels_agree_byte_for_byte() {
    assert_eq!(
        STREAM_UNKNOWN, LOGGING_UNKNOWN,
        "stream and logging sentinels must agree (stream: {STREAM_UNKNOWN:?}, logging: {LOGGING_UNKNOWN:?})"
    );
    assert_eq!(
        STREAM_UNKNOWN.as_bytes(),
        LOGGING_UNKNOWN.as_bytes(),
        "sentinel agreement must hold at the byte level"
    );
}

#[test]
fn valid_sha_passes_through_both_sides() {
    for sha in [VALID_SHA, VALID_SHA_ALL_F] {
        assert_eq!(sha.len(), 40, "fixture must be 40 chars: {sha:?}");
        assert_eq!(
            stream_normalize(Some(sha)),
            sha,
            "stream must pass a valid SHA through unchanged"
        );
        assert_eq!(
            logging_normalize(Some(sha)),
            sha,
            "logging must pass a valid SHA through unchanged"
        );
        assert!(
            stream_is_known(sha),
            "stream must report a valid SHA as known: {sha:?}"
        );
        assert!(
            logging_is_known(sha),
            "logging must report a valid SHA as known: {sha:?}"
        );
    }
}

#[test]
fn garbage_uppercase_and_overlong_map_to_agreed_sentinel() {
    // (candidate, label) pairs; `None` is the absent-candidate shape.
    let cases: &[(Option<&str>, &str)] = &[
        (None, "absent"),
        (Some(""), "empty"),
        (Some("unknown"), "sentinel input"),
        (Some("0123456789abcdef"), "16-char truncated"),
        (
            Some("0123456789abcdef0123456789abcdef0123456"),
            "39-char truncated",
        ),
        (Some(SHA_41), "41-char over-long"),
        (Some(SHA_64), "64-hex digest"),
        (Some(SHA_UPPER), "uppercase hex"),
        (Some(SHA_NON_HEX), "non-hex"),
        (Some("Bearer abc123"), "credential-like"),
    ];
    for (raw, label) in cases {
        let stream = stream_normalize(*raw);
        let logging = logging_normalize(*raw);
        assert_eq!(
            stream, STREAM_UNKNOWN,
            "{label} candidate must normalize to the stream sentinel, got {stream:?}"
        );
        assert_eq!(
            logging, LOGGING_UNKNOWN,
            "{label} candidate must normalize to the logging sentinel, got {logging:?}"
        );
        assert_eq!(
            stream, logging,
            "{label} candidate must normalize identically on both sides"
        );
        assert!(
            !stream_is_known(&stream),
            "{label} stream normalization must not be known"
        );
        assert!(
            !logging_is_known(&logging),
            "{label} logging normalization must not be known"
        );
    }
}

#[test]
fn sentinel_itself_is_not_known_on_both_sides() {
    assert!(!stream_is_known(STREAM_UNKNOWN));
    assert!(!logging_is_known(LOGGING_UNKNOWN));
    assert!(!stream_is_known("unknown"));
    assert!(!logging_is_known("unknown"));
    assert_eq!(stream_normalize(Some("unknown")), STREAM_UNKNOWN);
    assert_eq!(logging_normalize(Some("unknown")), LOGGING_UNKNOWN);
}

#[test]
fn invalid_input_is_never_echoed_back() {
    // Every invalid non-sentinel candidate must map to the sentinel, never
    // back to itself — the raw value is dropped, not trusted.
    let invalid: &[&str] = &[
        "",
        "0123456789abcdef",
        "0123456789abcdef0123456789abcdef0123456",
        SHA_41,
        SHA_64,
        SHA_UPPER,
        SHA_NON_HEX,
        "Bearer abc123",
        " 0123456789abcdef0123456789abcdef01234567",
        "0123456789abcdef0123456789abcdef01234567 ",
    ];
    for raw in invalid {
        let stream = stream_normalize(Some(*raw));
        let logging = logging_normalize(Some(*raw));
        assert_eq!(stream, STREAM_UNKNOWN);
        assert_eq!(logging, LOGGING_UNKNOWN);
        assert_ne!(
            stream, *raw,
            "stream must never echo invalid input back: {raw:?}"
        );
        assert_ne!(
            logging, *raw,
            "logging must never echo invalid input back: {raw:?}"
        );
        assert!(
            !stream_is_known(raw),
            "stream must not report invalid input as known: {raw:?}"
        );
        assert!(
            !logging_is_known(raw),
            "logging must not report invalid input as known: {raw:?}"
        );
    }
}
