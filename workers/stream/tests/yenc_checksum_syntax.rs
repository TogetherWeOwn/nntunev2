//! Acceptance tests: a *present but malformed* applicable yEnc checksum must
//! be rejected, not silently treated as omitted.
//!
//! The applicable (covering) checksum is:
//! - single-part: `crc32=`, with the existing compatible `pcrc32=` fallback
//!   when `crc32=` is absent;
//! - multipart: `pcrc32=` only. A `crc32=` on a part is the whole-file CRC
//!   and must never be compared against one part's bytes.
//!
//! Checksums stay optional: an absent applicable checksum keeps its existing
//! (size-checked) success behavior, a valid matching checksum succeeds, and
//! a valid mismatching checksum fails. But empty, non-hex, non-UTF-8, or
//! out-of-`u32`-range values in the applicable field are syntax errors: the
//! article must be rejected, and an invalid selected field must not fall
//! through to a different checksum field or to success.
//!
//! Deliberately unpinned (either behavior is acceptable to these tests):
//! short-but-valid hex (e.g. `crc32=abc`), a malformed non-applicable field
//! (multipart whole-file `crc32=`, single-part `pcrc32=` next to a valid
//! `crc32=`), and which error variant a malformed checksum produces —
//! malformed cases assert only `Err`, never a specific variant.
//!
//! All fixtures are tiny in-memory synthetic articles built with the public
//! encoder; only the checksum token on the `=yend` line is mutated. No
//! provider, R2, or runtime dependency.

use nntune_stream::yenc::{self, PartSpec, YencError};

const PAYLOAD: &[u8] = b"checksum syntax payload, long enough to crc meaningfully";

fn hex(v: u32) -> String {
    format!("{v:08x}")
}

fn matching() -> String {
    hex(yenc::crc32(PAYLOAD))
}

fn mismatching() -> String {
    hex(yenc::crc32(PAYLOAD) ^ 1)
}

/// Replace the value of the first `key=` token (on the `=yend` line these
/// fixtures carry exactly one) with `value`, byte-exactly.
fn replace_tag(article: &[u8], key: &str, value: &[u8]) -> Vec<u8> {
    let needle = format!("{key}=");
    let at = article
        .windows(needle.len())
        .position(|w| w == needle.as_bytes())
        .unwrap_or_else(|| panic!("tag {key}= present in fixture"));
    let val_start = at + needle.len();
    let val_end = article[val_start..]
        .iter()
        .position(|&b| b == b' ' || b == b'\t' || b == b'\r' || b == b'\n')
        .map(|p| val_start + p)
        .unwrap_or(article.len());
    let mut out = article[..val_start].to_vec();
    out.extend_from_slice(value);
    out.extend_from_slice(&article[val_end..]);
    out
}

/// Remove the ` key=value` token (including its leading space) entirely.
fn remove_tag(article: &[u8], key: &str) -> Vec<u8> {
    let needle = format!(" {key}=");
    let at = article
        .windows(needle.len())
        .position(|w| w == needle.as_bytes())
        .unwrap_or_else(|| panic!("tag {key}= present in fixture"));
    let val_end = article[at + needle.len()..]
        .iter()
        .position(|&b| b == b' ' || b == b'\t' || b == b'\r' || b == b'\n')
        .map(|p| at + needle.len() + p)
        .unwrap_or(article.len());
    let mut out = article[..at].to_vec();
    out.extend_from_slice(&article[val_end..]);
    out
}

/// Append ` key=value` to the `=yend` line.
fn add_tag(article: &[u8], key: &str, value: &[u8]) -> Vec<u8> {
    let yend = article
        .windows(5)
        .position(|w| w == b"=yend")
        .expect("=yend present in fixture");
    let line_end = article[yend..]
        .iter()
        .position(|&b| b == b'\r' || b == b'\n')
        .map(|p| yend + p)
        .expect("=yend line terminated");
    let mut out = article[..line_end].to_vec();
    out.extend_from_slice(format!(" {key}=").as_bytes());
    out.extend_from_slice(value);
    out.extend_from_slice(&article[line_end..]);
    out
}

fn single() -> Vec<u8> {
    yenc::encode(PAYLOAD, "c.bin")
}

fn multi(file_crc: Option<u32>) -> Vec<u8> {
    yenc::encode_part(
        PAYLOAD,
        "c.bin",
        PartSpec {
            file_size: (PAYLOAD.len() * 3) as u64,
            part: 2,
            total: 3,
            offset: PAYLOAD.len() as u64,
            file_crc,
        },
    )
}

fn assert_decodes_to_payload(article: &[u8]) {
    let got = yenc::decode_part(article).expect("article must decode");
    assert_eq!(got.data, PAYLOAD);
}

fn assert_rejected(article: &[u8]) {
    assert!(
        yenc::decode_part(article).is_err(),
        "article must be rejected, got Ok"
    );
}

// ---------------------------------------------------------------------------
// Single-part: positive compatibility controls (must keep passing)
// ---------------------------------------------------------------------------

#[test]
fn single_matching_crc32_succeeds() {
    assert_decodes_to_payload(&single());
    assert_eq!(yenc::decode(&single()).expect("decode"), PAYLOAD.to_vec());
}

#[test]
fn single_uppercase_matching_crc32_succeeds() {
    let article = replace_tag(&single(), "crc32", matching().to_uppercase().as_bytes());
    assert_decodes_to_payload(&article);
}

#[test]
fn single_mismatching_valid_crc32_fails() {
    let article = replace_tag(&single(), "crc32", mismatching().as_bytes());
    assert!(matches!(
        yenc::decode_part(&article),
        Err(YencError::CrcMismatch { .. })
    ));
}

#[test]
fn single_omitted_crc32_succeeds() {
    // Checksums are optional: absent is not malformed.
    assert_decodes_to_payload(&remove_tag(&single(), "crc32"));
}

#[test]
fn single_pcrc32_fallback_matching_succeeds() {
    // Existing compatibility: with crc32 absent, pcrc32 covers the data.
    let article = add_tag(
        &remove_tag(&single(), "crc32"),
        "pcrc32",
        matching().as_bytes(),
    );
    assert_decodes_to_payload(&article);
}

#[test]
fn single_pcrc32_fallback_mismatching_fails() {
    let article = add_tag(
        &remove_tag(&single(), "crc32"),
        "pcrc32",
        mismatching().as_bytes(),
    );
    assert!(matches!(
        yenc::decode_part(&article),
        Err(YencError::CrcMismatch { .. })
    ));
}

// ---------------------------------------------------------------------------
// Single-part: present-but-malformed crc32 must be rejected
// ---------------------------------------------------------------------------

#[test]
fn single_empty_crc32_is_rejected() {
    assert_rejected(&replace_tag(&single(), "crc32", b""));
}

#[test]
fn single_non_hex_crc32_is_rejected() {
    assert_rejected(&replace_tag(&single(), "crc32", b"bogus"));
}

#[test]
fn single_partially_hex_crc32_is_rejected() {
    assert_rejected(&replace_tag(&single(), "crc32", b"deadbeefzz"));
    assert_rejected(&replace_tag(&single(), "crc32", b"12xz5678"));
    assert_rejected(&replace_tag(&single(), "crc32", b"0x12345678"));
}

#[test]
fn single_leading_plus_crc32_is_rejected() {
    // `from_str_radix` accepts a leading `+`, so the plus carries the
    // otherwise-matching CRC: without the syntax guard this would decode.
    let plus_matching = format!("+{}", matching());
    assert_rejected(&replace_tag(&single(), "crc32", plus_matching.as_bytes()));
}

#[test]
fn single_non_utf8_crc32_is_rejected() {
    assert_rejected(&replace_tag(&single(), "crc32", &[0xff, 0xfe, 0xff, 0xfe]));
}

#[test]
fn single_overflowing_crc32_is_rejected() {
    // 0x1_0000_0000 and 0xFFFF_FFFF_F: valid hex digits, out of u32 range.
    assert_rejected(&replace_tag(&single(), "crc32", b"100000000"));
    assert_rejected(&replace_tag(&single(), "crc32", b"fffffffff"));
}

#[test]
fn single_malformed_crc32_does_not_fall_through_to_pcrc32() {
    // crc32 is the selected field; its bad syntax must not defer to a
    // perfectly good pcrc32 sitting next to it.
    let article = add_tag(&single(), "pcrc32", matching().as_bytes());
    assert_rejected(&replace_tag(&article, "crc32", b"bogus"));
    assert_rejected(&replace_tag(&article, "crc32", b""));
    assert_rejected(&replace_tag(&article, "crc32", b"100000000"));
}

#[test]
fn single_malformed_pcrc32_fallback_is_rejected() {
    // With crc32 absent, pcrc32 is the selected field: bad syntax there
    // must not be treated as "checksum omitted".
    let base = add_tag(
        &remove_tag(&single(), "crc32"),
        "pcrc32",
        matching().as_bytes(),
    );
    assert_rejected(&replace_tag(&base, "pcrc32", b"bogus"));
    assert_rejected(&replace_tag(&base, "pcrc32", b""));
    assert_rejected(&replace_tag(&base, "pcrc32", &[0xff, 0xfe]));
    assert_rejected(&replace_tag(&base, "pcrc32", b"100000000"));
}

// ---------------------------------------------------------------------------
// Multipart: positive compatibility controls (must keep passing)
// ---------------------------------------------------------------------------

#[test]
fn multipart_matching_pcrc32_succeeds() {
    assert_decodes_to_payload(&multi(None));
}

#[test]
fn multipart_mismatching_valid_pcrc32_fails() {
    let article = replace_tag(&multi(None), "pcrc32", mismatching().as_bytes());
    assert!(matches!(
        yenc::decode_part(&article),
        Err(YencError::CrcMismatch { .. })
    ));
}

#[test]
fn multipart_omitted_pcrc32_succeeds() {
    assert_decodes_to_payload(&remove_tag(&multi(None), "pcrc32"));
}

#[test]
fn multipart_whole_file_crc32_is_not_compared_to_the_part() {
    // Whole-file crc32 (valid hex, not the part's CRC) next to a valid
    // pcrc32: the part still decodes.
    assert_decodes_to_payload(&multi(Some(0xDEAD_BEEF)));
    // And with pcrc32 omitted, the whole-file crc32 does not become the
    // part's covering checksum either.
    assert_decodes_to_payload(&remove_tag(&multi(Some(0xDEAD_BEEF)), "pcrc32"));
}

// ---------------------------------------------------------------------------
// Multipart: present-but-malformed pcrc32 must be rejected
// ---------------------------------------------------------------------------

#[test]
fn multipart_empty_pcrc32_is_rejected() {
    assert_rejected(&replace_tag(&multi(None), "pcrc32", b""));
}

#[test]
fn multipart_non_hex_pcrc32_is_rejected() {
    assert_rejected(&replace_tag(&multi(None), "pcrc32", b"bogus"));
}

#[test]
fn multipart_partially_hex_pcrc32_is_rejected() {
    assert_rejected(&replace_tag(&multi(None), "pcrc32", b"deadbeefzz"));
    assert_rejected(&replace_tag(&multi(None), "pcrc32", b"0x12345678"));
}

#[test]
fn multipart_non_utf8_pcrc32_is_rejected() {
    assert_rejected(&replace_tag(
        &multi(None),
        "pcrc32",
        &[0xff, 0xfe, 0xff, 0xfe],
    ));
}

#[test]
fn multipart_overflowing_pcrc32_is_rejected() {
    assert_rejected(&replace_tag(&multi(None), "pcrc32", b"100000000"));
    assert_rejected(&replace_tag(&multi(None), "pcrc32", b"fffffffff"));
}

#[test]
fn multipart_malformed_pcrc32_does_not_fall_through_to_crc32() {
    // pcrc32 is the selected field for a part. Even when a crc32 token is
    // present whose value happens to equal the part CRC, a malformed pcrc32
    // must be rejected rather than resolved via crc32 or omission.
    let article = multi(Some(yenc::crc32(PAYLOAD)));
    assert_rejected(&replace_tag(&article, "pcrc32", b"bogus"));
    assert_rejected(&replace_tag(&article, "pcrc32", b""));
    assert_rejected(&replace_tag(&article, "pcrc32", b"100000000"));
}
