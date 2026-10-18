//! Acceptance tests: yEnc dangling `=` escapes and malformed framing fail
//! closed instead of decoding past them.
//!
//! - A body line ending in a bare `=` (dangling escape, `yenc.rs` guard)
//!   must be rejected with [`YencError::BadEscape`].
//! - Missing `=ybegin`, `=ypart`/`=yend` out of order or duplicated
//!   (the `:185,210,220` framing guards) must be rejected with
//!   [`YencError::BadFraming`].
//! - Well-formed single-part and multipart articles must still decode.
//!
//! All fixtures are tiny in-memory synthetic articles built with the public
//! encoder (plus byte-exact line moves); no provider, R2, or runtime
//! dependency.

use nntune_stream::yenc::{decode, decode_part, encode, encode_part, PartSpec, YencError};

const PAYLOAD: &[u8] = b"escape and framing payload 0123456789";

fn find(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
        .unwrap_or_else(|| {
            panic!(
                "needle {:?} not in article",
                String::from_utf8_lossy(needle)
            )
        })
}

fn multipart(data: &[u8], offset: u64, file_size: u64) -> Vec<u8> {
    encode_part(
        data,
        "m.bin",
        PartSpec {
            file_size,
            part: 1,
            total: 2,
            offset,
            file_crc: None,
        },
    )
}

// ---------------------------------------------------------------------------
// BadEscape: a body line ending in a bare `=` is a dangling escape.
// ---------------------------------------------------------------------------

#[test]
fn body_line_ending_in_bare_equals_is_bad_escape() {
    // Minimal hand-built article: the body line ends in a dangling `=`.
    let article = b"=ybegin line=128 size=3 name=a.bin\r\nABC=\r\n=yend size=3\r\n";
    assert_eq!(decode(article), Err(YencError::BadEscape));
}

#[test]
fn encoder_body_with_trailing_bare_equals_is_bad_escape() {
    // Encoder-built article with `=` appended to the end of the first body
    // line: the escape guard must fire before any size/CRC check.
    let article = encode(PAYLOAD, "a.bin");
    let first_crlf = find(&article, b"\r\n") + 2;
    let body_end = first_crlf + find(&article[first_crlf..], b"\r\n");
    let mut mutated = article[..body_end].to_vec();
    mutated.push(b'=');
    mutated.extend_from_slice(&article[body_end..]);
    assert_eq!(decode(&mutated), Err(YencError::BadEscape));
    assert_eq!(decode_part(&mutated), Err(YencError::BadEscape));
}

// ---------------------------------------------------------------------------
// BadFraming: missing begin, body-before-framing, and garbage framing.
// ---------------------------------------------------------------------------

#[test]
fn yend_without_begin_is_bad_framing() {
    // Missing `=ybegin`: a trailer with no header.
    let article = b"=yend size=3\r\n";
    assert_eq!(decode(article), Err(YencError::BadFraming));
}

#[test]
fn article_without_begin_is_bad_framing() {
    // Missing `=ybegin`: body plus trailer, header stripped.
    let article = encode(PAYLOAD, "a.bin");
    let first_crlf = find(&article, b"\r\n") + 2;
    assert_eq!(decode(&article[first_crlf..]), Err(YencError::BadFraming));
}

#[test]
fn ypart_before_begin_is_bad_framing() {
    // Body placement before the header: `=ypart` moved ahead of `=ybegin`.
    let article = multipart(PAYLOAD, 0, 1000);
    let ypart_start = find(&article, b"=ypart");
    let ypart_end = ypart_start + find(&article[ypart_start..], b"\r\n") + 2;
    let ypart = article[ypart_start..ypart_end].to_vec();
    let mut rest = article[..ypart_start].to_vec();
    rest.extend_from_slice(&article[ypart_end..]);
    let mut mutated = ypart;
    mutated.extend_from_slice(&rest);
    assert_eq!(decode_part(&mutated), Err(YencError::BadFraming));
}

#[test]
fn ypart_after_body_is_bad_framing() {
    // Body before `=ypart`: the placement record arrives after payload bytes.
    let article = multipart(PAYLOAD, 0, 1000);
    let ypart_start = find(&article, b"=ypart");
    let ypart_end = ypart_start + find(&article[ypart_start..], b"\r\n") + 2;
    let ypart = article[ypart_start..ypart_end].to_vec();
    let mut rest = article[..ypart_start].to_vec();
    rest.extend_from_slice(&article[ypart_end..]);
    let yend_at = find(&rest, b"=yend");
    let mut mutated = rest[..yend_at].to_vec();
    mutated.extend_from_slice(&ypart);
    mutated.extend_from_slice(&rest[yend_at..]);
    assert_eq!(decode_part(&mutated), Err(YencError::BadFraming));
}

#[test]
fn duplicate_ybegin_is_bad_framing() {
    // Garbage framing: a second `=ybegin` inside the article.
    let article = encode(PAYLOAD, "a.bin");
    let first_crlf = find(&article, b"\r\n") + 2;
    let header = article[..first_crlf].to_vec();
    let mut mutated = article[..first_crlf].to_vec();
    mutated.extend_from_slice(&header);
    mutated.extend_from_slice(&article[first_crlf..]);
    assert_eq!(decode(&mutated), Err(YencError::BadFraming));
}

#[test]
fn duplicate_trailer_is_bad_framing() {
    // Garbage framing: exactly one `=yend` per article.
    let mut article = encode(PAYLOAD, "a.bin");
    article.extend_from_slice(format!("=yend size={}\r\n", PAYLOAD.len()).as_bytes());
    assert_eq!(decode(&article), Err(YencError::BadFraming));
}

#[test]
fn ypart_after_trailer_is_bad_framing() {
    // Garbage framing: placement record after the trailer.
    let article = multipart(PAYLOAD, 0, 1000);
    let ypart_start = find(&article, b"=ypart");
    let ypart_end = ypart_start + find(&article[ypart_start..], b"\r\n") + 2;
    let mut mutated = article.clone();
    mutated.extend_from_slice(b"=ypart begin=1 end=10\r\n");
    let _ = &article[ypart_start..ypart_end];
    assert_eq!(decode_part(&mutated), Err(YencError::BadFraming));
}

// ---------------------------------------------------------------------------
// Positive controls: well-formed articles must still decode.
// ---------------------------------------------------------------------------

#[test]
fn control_well_formed_single_part_decodes() {
    let article = encode(PAYLOAD, "a.bin");
    assert_eq!(decode(&article).expect("decode single-part"), PAYLOAD);
}

#[test]
fn control_well_formed_multipart_decodes() {
    let article = multipart(PAYLOAD, 0, 1000);
    let got = decode_part(&article).expect("decode multipart");
    assert_eq!(got.data, PAYLOAD);
    assert_eq!((got.file_size, got.offset, got.multipart), (1000, 0, true));
}
