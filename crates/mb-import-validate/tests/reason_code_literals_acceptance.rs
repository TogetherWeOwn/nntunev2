//! Reason-code literal and row-mapping pins for `mb-validate`.
//!
//! `src/lib.rs:65-77` defines eleven `reason::` poison codes copied verbatim
//! into the dead-letter table. The in-module tests use those constants
//! symbolically, and the `exit_code_matrix` suite pins exit codes only
//! ("never message wording"), so a renamed literal would stay green. This
//! suite pins every literal with exact string equality and pins one
//! malformed row per `validate_line` class to its literal code with the
//! caller's `line_no` preserved, plus one valid row per table.
//!
//! Row-mapping asserts compare against string literals, not the `reason::`
//! symbols, so a code-string mutation fails here even though the constants
//! themselves moved. Sensitivity (2026-10-18): temporarily changed
//! `reason::BAD_UUID` to `"bad_uuid_x"`; `reason_literal_values` and
//! `bad_uuid_row_maps_to_literal` failed, then restored to green.
//!
//! `invalid_encoding` is the one code `validate_line` never emits: it comes
//! from the binary UTF-8 path in `src/main.rs:123-138`. Its literal is pinned
//! below and its row mapping is pinned through the real binary on `0xFF`
//! bytes, matching the `binary_byte_decoding` quarantine contract.

use mb_import_validate::{reason, validate_line, Table};
use std::process::Command;

const MBID: &str = "652b9a5c-ee58-41f5-bb9e-b0d0a5a7c9d1";
const MBID2: &str = "752b9a5c-ee58-41f5-bb9e-b0d0a5a7c9d2";
const TS: &str = "2024-05-01T12:00:00Z";

fn artist(mbid: &str, name: &str, country: &str, ts: &str) -> String {
    format!("{mbid}\t{name}\tBand, The\tGroup\t{country}\t{ts}")
}

fn valid_artist() -> String {
    artist(MBID, "The Band", "US", TS)
}

fn valid_release() -> String {
    format!("{MBID2}\t{MBID}\tAlbum Title\tAlbum\t2024\t\\N\t{TS}")
}

fn valid_recording() -> String {
    format!("{MBID}\t{MBID2}\tTrack One\t3\t180000\t{TS}")
}

fn valid_track() -> String {
    format!("{MBID2}\t{MBID}\t2\t0\t{TS}")
}

#[test]
fn reason_literal_values() {
    assert_eq!(reason::COLUMN_COUNT, "column_count");
    assert_eq!(reason::EMPTY_LINE, "empty_line");
    assert_eq!(reason::BAD_UUID, "bad_uuid");
    assert_eq!(reason::BAD_INT, "bad_int");
    assert_eq!(reason::BAD_TIMESTAMP, "bad_timestamp");
    assert_eq!(reason::BAD_ESCAPE, "bad_escape");
    assert_eq!(reason::NULL_IN_NOT_NULL, "null_in_not_null");
    assert_eq!(reason::BAD_COUNTRY, "bad_country");
    assert_eq!(reason::NEGATIVE_DURATION, "negative_duration");
    assert_eq!(reason::NEGATIVE_POSITION, "negative_position");
    assert_eq!(reason::INVALID_ENCODING, "invalid_encoding");
}

#[test]
fn short_column_row_maps_to_column_count_with_line_no() {
    let raw = format!("{MBID}\tOnly Two");
    assert_eq!(
        validate_line(Table::Artist, 42, &raw),
        Err(("column_count", 42))
    );
}

#[test]
fn empty_line_maps_to_empty_line_with_line_no() {
    assert_eq!(validate_line(Table::Artist, 7, ""), Err(("empty_line", 7)));
}

#[test]
fn bad_uuid_row_maps_to_literal() {
    let raw = artist("not-a-uuid", "The Band", "US", TS);
    assert_eq!(
        validate_line(Table::Artist, 11, &raw),
        Err(("bad_uuid", 11))
    );
}

#[test]
fn bad_int_row_maps_to_literal() {
    // Release year outside the i16 schema bound.
    let raw = format!("{MBID2}\t\\N\tT\tAlbum\t40000\t\\N\t{TS}");
    assert_eq!(
        validate_line(Table::Release, 12, &raw),
        Err(("bad_int", 12))
    );
}

#[test]
fn bad_timestamp_row_maps_to_literal() {
    let raw = artist(MBID, "The Band", "US", "not-a-date");
    assert_eq!(
        validate_line(Table::Artist, 13, &raw),
        Err(("bad_timestamp", 13))
    );
}

#[test]
fn bad_escape_row_maps_to_literal() {
    let raw = artist(MBID, "Bad \\q Name", "US", TS);
    assert_eq!(
        validate_line(Table::Artist, 14, &raw),
        Err(("bad_escape", 14))
    );
}

#[test]
fn null_in_not_null_row_maps_to_literal() {
    // Artist name is NOT NULL text: a verbatim COPY null marker poisons.
    let raw = format!("{MBID}\t\\N\tBand, The\tGroup\tUS\t{TS}");
    assert_eq!(
        validate_line(Table::Artist, 15, &raw),
        Err(("null_in_not_null", 15))
    );
}

#[test]
fn bad_country_row_maps_to_literal() {
    let raw = artist(MBID, "The Band", "USA", TS);
    assert_eq!(
        validate_line(Table::Artist, 16, &raw),
        Err(("bad_country", 16))
    );
}

#[test]
fn negative_duration_row_maps_to_literal() {
    let raw = format!("{MBID}\t{MBID2}\tT\t1\t-5\t{TS}");
    assert_eq!(
        validate_line(Table::Recording, 17, &raw),
        Err(("negative_duration", 17))
    );
}

#[test]
fn negative_position_row_maps_to_literal() {
    let raw = format!("{MBID2}\t{MBID}\t-1\t1\t{TS}");
    assert_eq!(
        validate_line(Table::Track, 18, &raw),
        Err(("negative_position", 18))
    );
}

#[test]
fn valid_row_per_table_passes_with_line_no_appended() {
    for (table, raw) in [
        (Table::Artist, valid_artist()),
        (Table::Release, valid_release()),
        (Table::Recording, valid_recording()),
        (Table::Track, valid_track()),
    ] {
        let want = format!("{raw}\t21");
        assert_eq!(validate_line(table, 21, &raw), Ok(want), "{table:?}");
    }
}

#[test]
fn invalid_encoding_row_maps_to_literal_through_binary() {
    // `validate_line` takes `&str`, so undecodable bytes only surface via the
    // real binary: `0xFF` is invalid UTF-8 and must poison with the literal
    // `invalid_encoding` code and the offset-adjusted line number.
    let dir = std::env::temp_dir().join(format!(
        "mb-validate-reason-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let input_path = dir.join("in.tsv");
    let valid_path = dir.join("valid.tsv");
    let poison_path = dir.join("poison.tsv");
    let mut input = Vec::new();
    input.extend_from_slice(b"bad-\xff-row");
    input.push(b'\n');
    std::fs::write(&input_path, &input).expect("write input fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_mb-validate"))
        .arg("--table")
        .arg("artist")
        .arg("--in")
        .arg(&input_path)
        .arg("--valid-out")
        .arg(&valid_path)
        .arg("--poison-out")
        .arg(&poison_path)
        .arg("--line-offset")
        .arg("50")
        .output()
        .expect("run mb-validate binary");
    assert!(
        output.status.success(),
        "poison quarantines with exit 0, got {} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let poison = std::fs::read(&poison_path).expect("read poison output");
    assert_eq!(
        poison,
        "invalid_encoding\t51\tbad-\u{FFFD}-row\n".as_bytes()
    );
    let _ = std::fs::remove_dir_all(&dir);
}
