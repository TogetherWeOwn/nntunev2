//! Acceptance tests: filename text must not change yEnc multipart
//! interpretation in the stream codec.
//!
//! Contract: on a `=ybegin` line, control tags (`size=`, `part=`, ...) are
//! only the whitespace-separated fields that appear *before* the `name=`
//! field. Everything from `name=` to the end of the line is filename text,
//! even when it contains spaces, tabs, or tokens that look like control
//! tags (`part=1`, `size=999`, ...). Filename text must therefore never:
//! - turn a single-part article into a multipart one (or vice versa), or
//! - satisfy control metadata that the real header fields are missing.
//!
//! Real control fields before `name=` keep working in any order and with
//! ordinary header whitespace (spaces or tabs, repeated separators), and
//! malformed real multipart metadata still fails. Latin-1 / non-UTF-8
//! filename bytes remain accepted.
//!
//! Positive compatibility controls are marked as such; the remaining
//! single-part cases pin the filename-boundary behavior itself.

use nntune_stream::yenc::{self, PartSpec, YencError};

/// Tiny deterministic payload covering every byte value (exercises escapes,
/// so "payload bytes unchanged" is a byte-exact assertion, not a length one).
fn payload() -> Vec<u8> {
    (0..=255u8).cycle().take(300).collect()
}

fn small_payload() -> Vec<u8> {
    b"tiny synthetic payload".to_vec()
}

/// Replace the first line (the `=ybegin` line) of an encoded article.
fn with_first_line(article: &[u8], new_first_line: &[u8]) -> Vec<u8> {
    let eol = article
        .iter()
        .position(|&b| b == b'\n')
        .expect("article has a first line");
    let mut out = new_first_line.to_vec();
    out.extend_from_slice(&article[eol..]);
    out
}

/// Replace a byte range found in the first line of an encoded article.
fn replace_in_first_line(article: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let eol = article
        .iter()
        .position(|&b| b == b'\n')
        .expect("article has a first line");
    let at = article[..eol]
        .windows(from.len())
        .position(|w| w == from)
        .expect("needle present in first line");
    let mut out = article.to_vec();
    out.splice(at..at + from.len(), to.iter().copied());
    out
}

/// Drop a token (including its trailing separator if present) from the
/// first line of an encoded article.
fn remove_from_first_line(article: &[u8], token: &[u8]) -> Vec<u8> {
    replace_in_first_line(article, token, b"")
}

/// Replace the encoded `name=<placeholder>` bytes anywhere in the header
/// with raw (possibly non-UTF-8) filename bytes.
fn splice_name_bytes(article: &[u8], placeholder: &[u8], raw_name: &[u8]) -> Vec<u8> {
    let from = [b"name=".as_slice(), placeholder].concat();
    let to = [b"name=".as_slice(), raw_name].concat();
    replace_in_first_line(article, &from, &to)
}

fn assert_single_part_roundtrip(name: &str) {
    let data = payload();
    let article = yenc::encode(&data, name);
    assert_eq!(
        yenc::decode(&article).expect("decode"),
        data,
        "decode() roundtrip for name {name:?}"
    );
    let got = yenc::decode_part(&article).expect("decode_part");
    assert_eq!(got.data, data, "payload bytes for name {name:?}");
    assert_eq!(
        (got.file_size, got.offset, got.multipart),
        (data.len() as u64, 0, false),
        "placement for name {name:?}"
    );
}

fn multipart_article(data: &[u8], name: &str, spec: PartSpec) -> Vec<u8> {
    yenc::encode_part(data, name, spec)
}

fn part_spec(file_size: u64, part: u32, total: u32, offset: u64) -> PartSpec {
    PartSpec {
        file_size,
        part,
        total,
        offset,
        file_crc: None,
    }
}

// ---------------------------------------------------------------------------
// Positive compatibility controls (must keep passing before and after)
// ---------------------------------------------------------------------------

/// Control: an ordinary spaceless filename round-trips as single-part.
#[test]
fn plain_name_without_spaces_round_trips_single_part() {
    assert_single_part_roundtrip("track01.flac");
}

/// Control: spaces in a filename with no tag-looking tokens are fine.
#[test]
fn filename_with_spaces_but_no_tag_tokens_round_trips_single_part() {
    assert_single_part_roundtrip("My Disc Track 01.flac");
}

/// Control: a UTF-8 filename with spaces round-trips as single-part.
#[test]
fn utf8_filename_with_spaces_round_trips_single_part() {
    assert_single_part_roundtrip("Björk — Jóga (live).flac");
}

/// Control: a filename whose first characters are a tag-looking token
/// glued to `name=` (no separator) is filename text.
#[test]
fn filename_starting_with_part_token_glued_to_name_round_trips() {
    assert_single_part_roundtrip("part=1 track.flac");
}

/// Control: a non-numeric `part=` token in the filename does not make the
/// article multipart.
#[test]
fn filename_with_non_numeric_part_token_stays_single_part() {
    assert_single_part_roundtrip("Disc part=one track.flac");
}

/// Control: a nested `name=` token inside the filename is filename text.
#[test]
fn filename_with_nested_name_token_round_trips_single_part() {
    assert_single_part_roundtrip("Disc name=part=1 track.flac");
}

/// Positive control: `size=`/`total=`/`line=` tokens in the filename do
/// not override the real header size; the article stays single-part with
/// the real declared size.
#[test]
fn filename_size_and_total_tokens_do_not_override_real_size() {
    assert_single_part_roundtrip("Disc size=999999 total=7 line=1 track.flac");
}

/// Control: a genuine multipart part with a plain filename decodes with
/// its placement.
#[test]
fn genuine_multipart_plain_name_keeps_placement() {
    let data = payload();
    let article = multipart_article(&data, "multi.bin", part_spec(3000, 2, 3, 1000));
    let got = yenc::decode_part(&article).expect("decode multipart");
    assert_eq!(got.data, data);
    assert_eq!(
        (got.file_size, got.offset, got.multipart),
        (3000, 1000, true)
    );
}

/// Positive control: a genuine multipart part whose filename contains
/// tag-looking tokens still decodes with the *real* placement (real
/// `part=`/`size=` before `name=` win).
#[test]
fn genuine_multipart_with_tag_looking_filename_keeps_placement() {
    let data = payload();
    let article = multipart_article(
        &data,
        "Disc part=9 size=1 total=99 track.flac",
        part_spec(3000, 2, 3, 1000),
    );
    let got = yenc::decode_part(&article).expect("decode multipart");
    assert_eq!(got.data, data);
    assert_eq!(
        (got.file_size, got.offset, got.multipart),
        (3000, 1000, true)
    );
}

/// Positive control: real multipart control fields before `name=` keep
/// working when reordered (e.g. `part=` after `line=`/`size=`).
#[test]
fn genuine_multipart_control_fields_reordered_before_name_still_work() {
    let data = payload();
    let article = multipart_article(&data, "multi.bin", part_spec(3000, 2, 3, 1000));
    let article = with_first_line(
        &article,
        b"=ybegin line=128 size=3000 part=2 total=3 name=multi.bin\r",
    );
    let got = yenc::decode_part(&article).expect("decode reordered multipart");
    assert_eq!(got.data, data);
    assert_eq!(
        (got.file_size, got.offset, got.multipart),
        (3000, 1000, true)
    );
}

/// Positive control: ordinary header whitespace — tabs and repeated
/// spaces between the real control fields (and before `name=`) — keeps
/// genuine multipart decoding working.
#[test]
fn tabs_and_repeated_spaces_between_real_control_fields_still_work() {
    let data = payload();
    let article = multipart_article(&data, "multi.bin", part_spec(3000, 2, 3, 1000));
    let article = with_first_line(
        &article,
        b"=ybegin\tpart=2  total=3\tline=128  size=3000\tname=multi.bin\r",
    );
    let got = yenc::decode_part(&article).expect("decode multipart with header whitespace");
    assert_eq!(got.data, data);
    assert_eq!(
        (got.file_size, got.offset, got.multipart),
        (3000, 1000, true)
    );
}

/// Positive control: Latin-1 / non-UTF-8 filename bytes with spaces keep
/// working on a genuine multipart part.
#[test]
fn latin1_multipart_filename_with_spaces_keeps_placement() {
    let data = payload();
    let article = multipart_article(&data, "multi.bin", part_spec(3000, 1, 3, 0));
    let article = splice_name_bytes(&article, b"multi.bin", b"Bj\xf6rk - J\xf3ga.flac");
    let got = yenc::decode_part(&article).expect("latin-1 multipart name must decode");
    assert_eq!(got.data, data);
    assert_eq!((got.file_size, got.offset, got.multipart), (3000, 0, true));
}

// ---------------------------------------------------------------------------
// Filename-boundary behavior (the regression being pinned)
// ---------------------------------------------------------------------------

/// A single-part article whose filename contains a `part=1` token must
/// decode as single-part with byte-exact payload — the encoder above can
/// emit exactly this article today.
#[test]
fn single_part_filename_with_part_token_round_trips() {
    assert_single_part_roundtrip("Disc part=1 track.flac");
}

/// Other part-looking filename tokens likewise cannot flip a single-part
/// article into a multipart one.
#[test]
fn single_part_filename_with_various_part_tokens_stays_single_part() {
    for name in [
        "CD part=0 bonus.flac",
        "Live part=23 (encore).flac",
        "a part=999999 b.flac",
        "Disc part=1 total=2 track.flac",
        "Disc total=2 part=2 track.flac",
    ] {
        assert_single_part_roundtrip(name);
    }
}

/// Tab-separated filename tokens are filename text too.
#[test]
fn single_part_filename_with_tab_separated_part_token_round_trips() {
    assert_single_part_roundtrip("Disc\tpart=1\ttrack.flac");
}

/// Repeated spaces before a part-looking filename token change nothing.
#[test]
fn single_part_filename_with_repeated_spaces_before_part_token_round_trips() {
    assert_single_part_roundtrip("Disc   part=1    track.flac");
}

/// A tab separator immediately before `name=` is ordinary header
/// whitespace; the part-looking text after it is still filename text.
#[test]
fn tab_before_name_and_part_token_in_filename_stays_single_part() {
    let data = small_payload();
    let article = yenc::encode(&data, "Disc part=1 track.flac");
    let article = replace_in_first_line(&article, b" name=", b"\tname=");
    let got = yenc::decode_part(&article).expect("decode single-part");
    assert_eq!(got.data, data);
    assert_eq!(
        (got.file_size, got.offset, got.multipart),
        (data.len() as u64, 0, false)
    );
}

/// Latin-1 filename bytes containing a part-looking token decode as
/// single-part (raw header bytes; not valid UTF-8).
#[test]
fn latin1_single_part_filename_with_part_token_round_trips() {
    let data = payload();
    let article = yenc::encode(&data, "track.flac");
    let article = splice_name_bytes(&article, b"track.flac", b"Bj\xf6rk part=1 J\xf3ga.flac");
    let got = yenc::decode_part(&article).expect("decode single-part latin-1");
    assert_eq!(got.data, data);
    assert_eq!(
        (got.file_size, got.offset, got.multipart),
        (data.len() as u64, 0, false)
    );
}

/// A `size=` token inside the filename must not satisfy a missing real
/// `size=` field: with the real size removed, the article is truncated /
/// incomplete even though the filename carries the matching number.
#[test]
fn filename_size_token_cannot_satisfy_missing_real_size_single_part() {
    let data = small_payload();
    let name = format!("Disc size={} track.flac", data.len());
    let article = yenc::encode(&data, &name);
    let real_size = format!("size={} ", data.len());
    let article = remove_from_first_line(&article, real_size.as_bytes());
    assert_eq!(
        yenc::decode_part(&article),
        Err(YencError::Truncated),
        "filename size= must not stand in for the real =ybegin size="
    );
}

/// Same for multipart: a `size=` token inside the filename must not
/// satisfy a missing real whole-file `size=` field.
#[test]
fn filename_size_token_cannot_satisfy_missing_real_size_multipart() {
    let data = payload();
    let file_size = 3000u64;
    let name = format!("Disc size={file_size} track.flac");
    let article = multipart_article(&data, &name, part_spec(file_size, 1, 3, 0));
    let real_size = format!("size={file_size} ");
    let article = remove_from_first_line(&article, real_size.as_bytes());
    assert_eq!(
        yenc::decode_part(&article),
        Err(YencError::Truncated),
        "filename size= must not stand in for the real =ybegin size="
    );
}

// ---------------------------------------------------------------------------
// Missing / malformed real control metadata (negative controls)
// ---------------------------------------------------------------------------

/// Control: with the real `size=` removed and no filename substitute, the
/// article is truncated.
#[test]
fn missing_real_size_without_filename_substitute_is_truncated() {
    let data = small_payload();
    let article = yenc::encode(&data, "track.flac");
    let real_size = format!("size={} ", data.len());
    let article = remove_from_first_line(&article, real_size.as_bytes());
    assert_eq!(yenc::decode_part(&article), Err(YencError::Truncated));
}

/// Negative control: a genuine multipart article without its `=ypart`
/// line is still `BadPart`.
#[test]
fn genuine_multipart_without_ypart_is_bad_part() {
    let data = small_payload();
    let article = multipart_article(&data, "multi.bin", part_spec(100, 1, 2, 0));
    let no_ypart: Vec<u8> = article
        .split(|&b| b == b'\n')
        .filter(|l| !l.starts_with(b"=ypart"))
        .flat_map(|l| [l, b"\n"].concat())
        .collect();
    assert_eq!(yenc::decode_part(&no_ypart), Err(YencError::BadPart));
}

/// Negative control: a genuine multipart article whose `=ypart` range is
/// malformed (non-numeric begin) is still `BadPart`.
#[test]
fn genuine_multipart_with_malformed_ypart_is_bad_part() {
    let data = small_payload();
    let mut article = multipart_article(&data, "multi.bin", part_spec(100, 1, 2, 0));
    let at = article
        .windows(b"begin=".len())
        .position(|w| w == b"begin=")
        .expect("ypart begin tag present");
    article[at + b"begin=".len()] = b'x';
    assert_eq!(yenc::decode_part(&article), Err(YencError::BadPart));
}

/// Negative control: a genuine multipart article whose `=ypart` range
/// length disagrees with the payload is still `BadPart`.
#[test]
fn genuine_multipart_with_inconsistent_ypart_length_is_bad_part() {
    let data = b"abcdef".to_vec();
    let mut article = multipart_article(&data, "multi.bin", part_spec(6, 1, 1, 0));
    let at = article
        .windows(b"end=6".len())
        .position(|w| w == b"end=6")
        .expect("ypart end tag present");
    article[at + b"end=".len()] = b'9';
    assert_eq!(yenc::decode_part(&article), Err(YencError::BadPart));
}

/// Negative control: a malformed real `part=` value before `name=` does
/// not decode as a valid article.
#[test]
fn malformed_real_part_value_before_name_still_fails() {
    let data = small_payload();
    let article = multipart_article(&data, "multi.bin", part_spec(100, 1, 2, 0));
    let article = replace_in_first_line(&article, b"part=1", b"part=x");
    assert!(
        yenc::decode_part(&article).is_err(),
        "malformed real part= metadata must not decode successfully"
    );
}
