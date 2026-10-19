//! Acceptance tests: single-part yEnc checksum precedence is
//! `crc32=` then `pcrc32=` (matching the stream decoder comment).
//!
//! A single-part article carrying both checksums with disagreeing values
//! (matching `crc32=`, mismatching `pcrc32=`) must decode: the whole-file
//! `crc32=` covers the payload here (part == whole file), so it governs and
//! the disagreeing `pcrc32=` must not veto it. Field order in the trailer
//! must not matter.
//!
//! Multipart articles stay `pcrc32=`-only: a matching `pcrc32=` decodes even
//! next to a differing whole-file `crc32=`, and a mismatching `pcrc32=` is
//! rejected even when a `crc32=` that equals the part CRC is present.
//! A present-but-malformed selected field fails closed (no fall-through to
//! the other field or to success).
//!
//! Fixtures are tiny in-memory synthetic articles built with the public
//! encoder; only the `=yend` checksum tokens are mutated. No provider, R2,
//! or runtime dependency.

use nntune_stream::yenc::{self, PartSpec};

const PAYLOAD: &[u8] = b"single-part crc precedence payload, crc meaningful";

fn hex(v: u32) -> String {
    format!("{v:08x}")
}

fn matching() -> String {
    hex(yenc::crc32(PAYLOAD))
}

fn mismatching() -> String {
    hex(yenc::crc32(PAYLOAD) ^ 1)
}

/// Replace the value of the first `key=` token with `value`, byte-exactly.
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

/// Single-part article with both checksums disagreeing: `crc32=` matches the
/// payload, `pcrc32=` does not. The `crc_order` flag selects trailer order.
fn single_both_disagreeing(crc_first: bool) -> Vec<u8> {
    if crc_first {
        // Encoder default is `crc32=`; appending gives `crc32= pcrc32=`.
        add_tag(&single(), "pcrc32", mismatching().as_bytes())
    } else {
        // Rebuild as `pcrc32= crc32=`.
        let base = remove_tag(&single(), "crc32");
        let base = add_tag(&base, "pcrc32", mismatching().as_bytes());
        add_tag(&base, "crc32", matching().as_bytes())
    }
}

fn assert_decodes_to_payload(article: &[u8], label: &str) {
    let got = yenc::decode_part(article)
        .unwrap_or_else(|e| panic!("{label}: expected successful decode, got {e:?}"));
    assert_eq!(got.data, PAYLOAD, "{label}: decoded bytes");
}

fn assert_rejected(article: &[u8], label: &str) {
    assert!(
        yenc::decode_part(article).is_err(),
        "{label}: article must be rejected, got Ok"
    );
}

#[test]
fn disagreeing_checksums_differ() {
    assert_ne!(
        matching(),
        mismatching(),
        "fixture premise: crc32 and pcrc32 values must disagree"
    );
}

#[test]
fn single_matching_crc_with_mismatching_pcrc_decodes_crc_first() {
    assert_decodes_to_payload(
        &single_both_disagreeing(true),
        "single-part crc32 then pcrc32, crc32 matches",
    );
}

#[test]
fn single_matching_crc_with_mismatching_pcrc_decodes_pcrc_first() {
    assert_decodes_to_payload(
        &single_both_disagreeing(false),
        "single-part pcrc32 then crc32, crc32 matches",
    );
}

#[test]
fn single_mismatching_crc_with_matching_pcrc_is_rejected() {
    // `crc32=` is the selected single-part field: a mismatching `crc32=`
    // must be rejected even when `pcrc32=` matches (no fall-through to the
    // other field). Replace `crc32=` before adding `pcrc32=` so the `crc32=`
    // needle cannot match inside `pcrc32=`.
    let crc_first = add_tag(
        &replace_tag(&single(), "crc32", mismatching().as_bytes()),
        "pcrc32",
        matching().as_bytes(),
    );
    assert_rejected(
        &crc_first,
        "single-part mismatching crc32 with matching pcrc32, crc first",
    );
    // Rebuild as `pcrc32=` then `crc32=` order.
    let base = remove_tag(&single(), "crc32");
    let base = add_tag(&base, "pcrc32", matching().as_bytes());
    let pcrc_first = add_tag(&base, "crc32", mismatching().as_bytes());
    assert_rejected(
        &pcrc_first,
        "single-part mismatching crc32 with matching pcrc32, pcrc first",
    );
}

#[test]
fn multipart_matching_pcrc_with_differing_whole_crc_decodes() {
    // `pcrc32=` governs the part; the whole-file `crc32=` must not veto it.
    let whole = 0xDEAD_BEEFu32;
    assert_ne!(whole, yenc::crc32(PAYLOAD), "whole-file stand-in differs");
    assert_decodes_to_payload(&multi(Some(whole)), "multipart matching pcrc32");
    // Same with `pcrc32=` omitted: the whole-file value is not compared.
    assert_decodes_to_payload(
        &remove_tag(&multi(Some(whole)), "pcrc32"),
        "multipart whole-file crc32 only",
    );
}

#[test]
fn multipart_mismatching_pcrc_with_matching_crc_is_rejected() {
    // Even a `crc32=` that equals the part CRC must not rescue a part whose
    // `pcrc32=` mismatches.
    let article = add_tag(&multi(None), "crc32", matching().as_bytes());
    let article = replace_tag(&article, "pcrc32", mismatching().as_bytes());
    assert_rejected(&article, "multipart mismatching pcrc32 with part-crc crc32");
}

#[test]
fn single_malformed_crc_with_valid_pcrc_is_rejected() {
    // `crc32=` is the selected single-part field: bad syntax there must not
    // fall through to a good `pcrc32=` next to it.
    let base = add_tag(&single(), "pcrc32", matching().as_bytes());
    assert_rejected(
        &replace_tag(&base, "crc32", b"bogus"),
        "single crc32 non-hex",
    );
    assert_rejected(&replace_tag(&base, "crc32", b""), "single crc32 empty");
    assert_rejected(
        &replace_tag(&base, "crc32", b"100000000"),
        "single crc32 overflow",
    );
}

#[test]
fn multipart_malformed_pcrc_with_valid_crc_is_rejected() {
    // `pcrc32=` is the selected multipart field: bad syntax there must not
    // fall through to `crc32=` or to success.
    let article = multi(Some(yenc::crc32(PAYLOAD)));
    assert_rejected(
        &replace_tag(&article, "pcrc32", b"bogus"),
        "multipart pcrc32 non-hex",
    );
    assert_rejected(
        &replace_tag(&article, "pcrc32", b""),
        "multipart pcrc32 empty",
    );
    assert_rejected(
        &replace_tag(&article, "pcrc32", b"100000000"),
        "multipart pcrc32 overflow",
    );
}
