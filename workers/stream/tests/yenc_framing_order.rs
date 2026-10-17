//! Acceptance tests: a single article has exactly one framing sequence
//! (`=ybegin`, optional `=ypart`, body, `=yend`). Duplicate or out-of-order
//! framing records must be rejected instead of overwriting already-accepted
//! size, placement, or checksum state — in particular, a trailer carrying a
//! bad CRC followed by a second, CRC-less trailer must not decode.
//!
//! Test-only: these tests use the public encoder to build small synthetic
//! articles and then insert/repeat/move framing lines. They assert only
//! `Err` (never a specific new error variant) so the implementation may
//! choose how to classify framing-order failures. Positive controls pin the
//! compatibility that must survive: normal single/multipart articles, a
//! CRC-less trailer (no new mandatory-checksum policy), NNTP preamble and
//! non-framing epilogue lines, LF-only line endings, and non-UTF8
//! (Latin-1) filenames.

use nntune_stream::yenc::{crc32, decode, decode_part, encode, encode_part, PartSpec};

const PAYLOAD: &[u8] = b"framing order payload, distinct bytes 0123456789";

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

/// The article with `line` inserted immediately after its first CRLF line.
fn after_first_line(article: &[u8], line: &[u8]) -> Vec<u8> {
    let at = find(article, b"\r\n") + 2;
    let mut out = article[..at].to_vec();
    out.extend_from_slice(line);
    out.extend_from_slice(&article[at..]);
    out
}

/// The `=ypart ...` line (without its CRLF) of an encoder-produced part.
fn ypart_line(article: &[u8]) -> Vec<u8> {
    let start = find(article, b"=ypart");
    let end = start + find(&article[start..], b"\r\n");
    article[start..end].to_vec()
}

/// The article with its `=ypart` line (and CRLF) removed.
fn without_ypart(article: &[u8]) -> Vec<u8> {
    let start = find(article, b"=ypart");
    let end = start + find(&article[start..], b"\r\n") + 2;
    let mut out = article[..start].to_vec();
    out.extend_from_slice(&article[end..]);
    out
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
// Rejection cases: later framing must not replace accepted metadata.
// ---------------------------------------------------------------------------

#[test]
fn bad_crc_trailer_followed_by_crc_less_trailer_cannot_succeed() {
    let mut article = encode(PAYLOAD, "a.bin");
    let good_crc = format!("{:08x}", crc32(PAYLOAD));
    let bad_crc = format!("{:08x}", crc32(PAYLOAD) ^ 1);
    let at = find(&article, good_crc.as_bytes());
    article.splice(at..at + good_crc.len(), bad_crc.bytes());
    // Alone, the bad-CRC trailer already fails (existing behaviour).
    assert!(decode(&article).is_err(), "bad CRC alone must fail");
    // A second, correctly sized trailer without a CRC must not erase that.
    article.extend_from_slice(format!("=yend size={}\r\n", PAYLOAD.len()).as_bytes());
    assert!(
        decode(&article).is_err(),
        "second CRC-less trailer must not turn a CRC failure into success"
    );
}

#[test]
fn bad_pcrc_part_trailer_followed_by_crc_less_trailer_cannot_succeed() {
    let mut article = multipart(PAYLOAD, 0, 1000);
    let good_pcrc = format!("pcrc32={:08x}", crc32(PAYLOAD));
    let bad_pcrc = format!("pcrc32={:08x}", crc32(PAYLOAD) ^ 1);
    let at = find(&article, good_pcrc.as_bytes());
    article.splice(at..at + good_pcrc.len(), bad_pcrc.bytes());
    assert!(decode_part(&article).is_err(), "bad pcrc32 alone must fail");
    article.extend_from_slice(format!("=yend size={} part=1\r\n", PAYLOAD.len()).as_bytes());
    assert!(
        decode_part(&article).is_err(),
        "second pcrc-less trailer must not turn a pcrc failure into success"
    );
}

#[test]
fn duplicate_trailer_after_good_trailer_is_rejected() {
    let mut article = encode(PAYLOAD, "a.bin");
    article.extend_from_slice(
        format!(
            "=yend size={} crc32={:08x}\r\n",
            PAYLOAD.len(),
            crc32(PAYLOAD)
        )
        .as_bytes(),
    );
    assert!(
        decode(&article).is_err(),
        "an article has exactly one trailer"
    );
}

#[test]
fn duplicate_ybegin_inside_article_is_rejected() {
    let article = encode(PAYLOAD, "a.bin");
    let second_begin =
        format!("=ybegin line=128 size={} name=other.bin\r\n", PAYLOAD.len()).into_bytes();
    let mutated = after_first_line(&article, &second_begin);
    assert!(
        decode(&mutated).is_err(),
        "a second =ybegin must not replace the accepted header"
    );
}

#[test]
fn ybegin_after_trailer_is_rejected() {
    let mut article = encode(PAYLOAD, "a.bin");
    article.extend_from_slice(
        format!("=ybegin line=128 size={} name=other.bin\r\n", PAYLOAD.len()).as_bytes(),
    );
    assert!(
        decode(&article).is_err(),
        "framing after the trailer is not a non-framing epilogue"
    );
}

#[test]
fn ypart_before_ybegin_is_rejected() {
    let article = multipart(PAYLOAD, 0, 1000);
    let mut mutated = ypart_line(&article);
    mutated.extend_from_slice(b"\r\n");
    mutated.extend_from_slice(&without_ypart(&article));
    assert!(
        decode_part(&mutated).is_err(),
        "=ypart before =ybegin is out of order"
    );
}

#[test]
fn ypart_after_body_is_rejected() {
    let article = multipart(PAYLOAD, 0, 1000);
    let rest = without_ypart(&article);
    let yend_at = find(&rest, b"=yend");
    let mut mutated = rest[..yend_at].to_vec();
    mutated.extend_from_slice(&ypart_line(&article));
    mutated.extend_from_slice(b"\r\n");
    mutated.extend_from_slice(&rest[yend_at..]);
    assert!(
        decode_part(&mutated).is_err(),
        "=ypart after the body must not retroactively place the payload"
    );
}

#[test]
fn duplicate_ypart_is_rejected() {
    let article = multipart(PAYLOAD, 0, 1000);
    let mut dupe = ypart_line(&article);
    dupe.extend_from_slice(b"\r\n");
    // Insert the repeated record right after the real one (second line).
    let first_crlf = find(&article, b"\r\n") + 2;
    let insert_at = first_crlf + find(&article[first_crlf..], b"\r\n") + 2;
    let mut mutated = article[..insert_at].to_vec();
    mutated.extend_from_slice(&dupe);
    mutated.extend_from_slice(&article[insert_at..]);
    assert!(
        decode_part(&mutated).is_err(),
        "an article has exactly one =ypart record"
    );
}

#[test]
fn second_ypart_replacing_placement_is_rejected() {
    let article = multipart(PAYLOAD, 0, 1000);
    // Same length, shifted placement: accepting it would silently move the
    // payload from offset 0 to offset 100.
    let shifted = format!("=ypart begin=101 end={}\r\n", 100 + PAYLOAD.len()).into_bytes();
    let first_crlf = find(&article, b"\r\n") + 2;
    let insert_at = first_crlf + find(&article[first_crlf..], b"\r\n") + 2;
    let mut mutated = article[..insert_at].to_vec();
    mutated.extend_from_slice(&shifted);
    mutated.extend_from_slice(&article[insert_at..]);
    assert!(
        decode_part(&mutated).is_err(),
        "a later =ypart must not replace accepted placement"
    );
}

#[test]
fn framing_order_errors_are_deterministic_and_do_not_expose_payload() {
    let mut article = encode(PAYLOAD, "a.bin");
    article.extend_from_slice(format!("=yend size={}\r\n", PAYLOAD.len()).as_bytes());
    let first = decode(&article).expect_err("duplicate trailer must be rejected");
    let second = decode(&article).expect_err("duplicate trailer must be rejected");
    assert_eq!(first, second, "same malformed input, same error");
    let rendered = format!("{first:?}");
    assert!(
        !rendered.contains("framing order payload"),
        "error diagnostics must not embed payload data: {rendered}"
    );
}

// ---------------------------------------------------------------------------
// Positive compatibility controls: these must keep decoding.
// ---------------------------------------------------------------------------

#[test]
fn control_single_part_round_trip() {
    let article = encode(PAYLOAD, "a.bin");
    assert_eq!(decode(&article).expect("decode single-part"), PAYLOAD);
}

#[test]
fn control_single_part_without_crc_still_decodes() {
    // No new mandatory-checksum policy: a size-only trailer is accepted.
    let article = encode(PAYLOAD, "a.bin");
    let crc_at = find(&article, b" crc32=");
    let trailer_end = crc_at + find(&article[crc_at..], b"\r\n");
    let mut stripped = article[..crc_at].to_vec();
    stripped.extend_from_slice(&article[trailer_end..]);
    assert_eq!(decode(&stripped).expect("decode CRC-less"), PAYLOAD);
}

#[test]
fn control_multipart_round_trip_with_placement() {
    let article = multipart(PAYLOAD, 1000, 3000);
    let got = decode_part(&article).expect("decode multipart");
    assert_eq!(got.data, PAYLOAD);
    assert_eq!(
        (got.file_size, got.offset, got.multipart),
        (3000, 1000, true)
    );
}

#[test]
fn control_nntp_preamble_before_ybegin_decodes() {
    let mut article =
        b"220 0 <synthetic@example.invalid> article\r\nSubject: synthetic\r\n\r\n".to_vec();
    article.extend_from_slice(&encode(PAYLOAD, "a.bin"));
    assert_eq!(decode(&article).expect("decode with preamble"), PAYLOAD);
}

#[test]
fn control_non_framing_epilogue_after_yend_decodes() {
    let mut article = encode(PAYLOAD, "a.bin");
    article.extend_from_slice(b"posted with a synthetic newsreader\r\n.\r\n");
    assert_eq!(decode(&article).expect("decode with epilogue"), PAYLOAD);
}

#[test]
fn control_lf_only_line_endings_decode() {
    let crlf = multipart(PAYLOAD, 0, 1000);
    // Byte-wise CRLF -> LF (body bytes may contain a bare CR only escaped,
    // so every literal CRLF pair here is a line ending).
    let mut lf = Vec::with_capacity(crlf.len());
    let mut i = 0;
    while i < crlf.len() {
        if crlf[i] == b'\r' && crlf.get(i + 1) == Some(&b'\n') {
            lf.push(b'\n');
            i += 2;
        } else {
            lf.push(crlf[i]);
            i += 1;
        }
    }
    let got = decode_part(&lf).expect("decode LF-only multipart");
    assert_eq!(got.data, PAYLOAD);
    assert_eq!((got.file_size, got.offset, got.multipart), (1000, 0, true));
}

#[test]
fn control_non_utf8_filename_decodes() {
    let mut article = multipart(PAYLOAD, 0, 1000);
    let from = b"name=m.bin";
    let at = find(&article, from);
    article.splice(
        at..at + from.len(),
        b"name=Bj\xf6rk - Joga.flac".iter().copied(),
    );
    let got = decode_part(&article).expect("latin-1 name must decode");
    assert_eq!(got.data, PAYLOAD);
}
