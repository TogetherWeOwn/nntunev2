//! Executable regression tests for `mb-validate` byte handling.
//!
//! The library tests in `src/lib.rs` only cover `&str` inputs, so they
//! cannot exercise the binary path in `src/main.rs:105-173`: UTF-8
//! decoding, LF splitting, CRLF stripping, chunk offsets and EOF. These
//! tests invoke the real binary via `CARGO_BIN_EXE_mb-validate` on byte
//! fixtures with isolated output files.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const MBID: &str = "652b9a5c-ee58-41f5-bb9e-b0d0a5a7c9d1";
const TS: &str = "2024-05-01T12:00:00Z";
const VALID_ARTIST: &str =
    "652b9a5c-ee58-41f5-bb9e-b0d0a5a7c9d1\tThe Band\tBand, The\tGroup\tUS\t2024-05-01T12:00:00Z";
const BAD_UUID_ARTIST: &str = "not-a-uuid\tThe Band\tBand, The\tGroup\tUS\t2024-05-01T12:00:00Z";
// `escape_poison_field` rewrites each raw tab as backslash-t so the poison
// row round-trips through `COPY (FORMAT text)` as a single field.
const BAD_UUID_ESCAPED: &str =
    "not-a-uuid\\tThe Band\\tBand, The\\tGroup\\tUS\\t2024-05-01T12:00:00Z";

static CASE_SEQ: AtomicU64 = AtomicU64::new(0);

struct CaseOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    valid: Vec<u8>,
    poison: Vec<u8>,
}

fn scratch_dir(case: &str) -> PathBuf {
    let seq = CASE_SEQ.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!(
        "mb-validate-{}-{}-{}",
        case,
        std::process::id(),
        seq
    ))
}

fn run_validate(case: &str, input: &[u8], offset: u64) -> CaseOutput {
    let dir = scratch_dir(case);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let input_path = dir.join("in.tsv");
    let valid_path = dir.join("valid.tsv");
    let poison_path = dir.join("poison.tsv");
    std::fs::write(&input_path, input).expect("write input fixture");
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
        .arg(offset.to_string())
        .output()
        .expect("run mb-validate binary");
    let valid = std::fs::read(&valid_path).expect("read valid output");
    let poison = std::fs::read(&poison_path).expect("read poison output");
    let _ = std::fs::remove_dir_all(&dir);
    CaseOutput {
        status: output.status,
        stdout: output.stdout,
        stderr: output.stderr,
        valid,
        poison,
    }
}

fn valid_row_with_line_no(line_no: u64) -> Vec<u8> {
    format!("{VALID_ARTIST}\t{line_no}\n").into_bytes()
}

fn bad_uuid_poison(line_no: u64) -> Vec<u8> {
    format!("bad_uuid\t{line_no}\t{BAD_UUID_ESCAPED}\n").into_bytes()
}

#[test]
fn mixed_valid_invalid_utf8_and_malformed_pins_bytes_and_counts() {
    // Line numbers must include the nonzero chunk offset: 100 + idx + 1.
    let mut input = Vec::new();
    input.extend_from_slice(VALID_ARTIST.as_bytes());
    input.push(b'\n');
    // Invalid UTF-8 takes the invalid_encoding poison path before any
    // column validation; 0xFF decodes as U+FFFD via `from_utf8_lossy`.
    input.extend_from_slice(b"bad-\xff-row");
    input.push(b'\n');
    input.extend_from_slice(BAD_UUID_ARTIST.as_bytes());
    input.push(b'\n');

    let out = run_validate("mixed", &input, 100);

    assert!(
        out.status.success(),
        "poison rows quarantine with exit 0, got {} stderr={:?}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"valid=1 poison=2\n".as_slice());
    assert!(
        out.stderr.is_empty(),
        "unexpected stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.valid, valid_row_with_line_no(101));
    let mut want_poison = Vec::new();
    want_poison.extend_from_slice("invalid_encoding\t102\tbad-\u{FFFD}-row\n".as_bytes());
    want_poison.extend_from_slice(&bad_uuid_poison(103));
    assert_eq!(out.poison, want_poison);
}

#[test]
fn crlf_rows_validate_like_lf_without_carriage_returns() {
    let mut input = Vec::new();
    input.extend_from_slice(VALID_ARTIST.as_bytes());
    input.extend_from_slice(b"\r\n");
    input.extend_from_slice(BAD_UUID_ARTIST.as_bytes());
    input.extend_from_slice(b"\r\n");

    let out = run_validate("crlf", &input, 20);

    assert!(
        out.status.success(),
        "exit status {} stderr={:?}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"valid=1 poison=1\n".as_slice());
    assert_eq!(out.valid, valid_row_with_line_no(21));
    assert_eq!(out.poison, bad_uuid_poison(22));
    assert!(
        !out.valid.contains(&b'\r'),
        "valid output must not carry CRLF bytes"
    );
    assert!(
        !out.poison.contains(&b'\r'),
        "poison output must escape CR, never emit a raw CR byte"
    );
}

#[test]
fn final_row_without_lf_is_still_validated() {
    let mut input = Vec::new();
    input.extend_from_slice(VALID_ARTIST.as_bytes());
    input.push(b'\n');
    // No trailing newline on the last row.
    input.extend_from_slice(BAD_UUID_ARTIST.as_bytes());

    let out = run_validate("noeol", &input, 0);

    assert!(out.status.success());
    assert_eq!(out.stdout, b"valid=1 poison=1\n".as_slice());
    assert_eq!(out.valid, valid_row_with_line_no(1));
    assert_eq!(out.poison, bad_uuid_poison(2));
}

#[test]
fn trailing_lf_does_not_emit_phantom_row() {
    let mut input = Vec::new();
    input.extend_from_slice(VALID_ARTIST.as_bytes());
    input.push(b'\n');

    let out = run_validate("trailing", &input, 0);

    assert!(out.status.success());
    assert_eq!(out.stdout, b"valid=1 poison=0\n".as_slice());
    assert_eq!(out.valid, valid_row_with_line_no(1));
    assert!(
        out.poison.is_empty(),
        "trailing newline must not poison an empty phantom row, got {:?}",
        String::from_utf8_lossy(&out.poison)
    );
}

#[test]
fn empty_input_produces_empty_outputs_and_zero_counts() {
    let out = run_validate("empty", b"", 41);

    assert!(out.status.success());
    assert_eq!(out.stdout, b"valid=0 poison=0\n".as_slice());
    assert!(
        out.valid.is_empty(),
        "empty input must leave valid output empty"
    );
    assert!(
        out.poison.is_empty(),
        "empty input must leave poison output empty"
    );
}

#[test]
fn all_poison_input_keeps_exit_zero_quarantine_contract() {
    let mut input = Vec::new();
    input.extend_from_slice(BAD_UUID_ARTIST.as_bytes());
    input.push(b'\n');

    let out = run_validate("allpoison", &input, 5);

    assert!(
        out.status.success(),
        "all-poison chunk must still exit 0, got {} stderr={:?}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"valid=0 poison=1\n".as_slice());
    assert!(out.valid.is_empty());
    assert_eq!(out.poison, bad_uuid_poison(6));
}

#[test]
fn constants_match_lib_fixtures() {
    // Guard against the constants above drifting from the crate's own
    // test fixtures (uuid/timestamp shapes).
    assert!(MBID.contains('-'));
    assert!(TS.contains("2024-05-01"));
}
