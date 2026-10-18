//! Exit-code regression matrix for `mb-validate`.
//!
//! The library tests pin validation and the `binary_byte_decoding` suite
//! pins exit-0 decoding; this suite pins the exit-2 side: every usage or
//! I/O failure class must exit 2 via the real binary. Poison rows stay
//! exit 0 (see `binary_byte_decoding.rs`) and are not re-pinned here.
//!
//! Classes (see `src/main.rs`): no-args usage, missing/invalid numbers,
//! `--size 0`, missing/unreadable input, and row/flush write failures
//! where simulable (`/dev/full`, missing parents, directory outputs).
//! Only the exit code is asserted, never message wording.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const VALID_ARTIST: &str =
    "652b9a5c-ee58-41f5-bb9e-b0d0a5a7c9d1\tThe Band\tBand, The\tGroup\tUS\t2024-05-01T12:00:00Z";
const BAD_UUID_ARTIST: &str = "not-a-uuid\tThe Band\tBand, The\tGroup\tUS\t2024-05-01T12:00:00Z";

static CASE_SEQ: AtomicU64 = AtomicU64::new(0);

fn scratch_dir(case: &str) -> PathBuf {
    let seq = CASE_SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "mb-validate-exit2-{case}-{}-{seq}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_mb-validate")
}

fn run_raw(args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .output()
        .expect("run mb-validate binary")
}

fn assert_exit_2(output: &Output, case: &str) {
    assert_eq!(
        output.status.code(),
        Some(2),
        "{case}: expected exit code 2, got {} stderr={:?} stdout={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
}

/// Full validate argv for one scratch dir; caller chooses the input bytes
/// and whether outputs live in the scratch dir or on a failing path.
fn validate_args(
    table: &str,
    input: &std::path::Path,
    valid_out: &std::path::Path,
    poison_out: &std::path::Path,
    line_offset: &str,
) -> Vec<String> {
    vec![
        "--table".to_string(),
        table.to_string(),
        "--in".to_string(),
        input.to_string_lossy().into_owned(),
        "--valid-out".to_string(),
        valid_out.to_string_lossy().into_owned(),
        "--poison-out".to_string(),
        poison_out.to_string_lossy().into_owned(),
        "--line-offset".to_string(),
        line_offset.to_string(),
    ]
}

fn run_validate_argv(argv: Vec<String>) -> Output {
    Command::new(bin())
        .args(&argv)
        .output()
        .expect("run mb-validate binary")
}

#[test]
fn no_args_usage_exits_2() {
    let output = run_raw(&[]);
    assert_exit_2(&output, "no args");
}

#[test]
fn plan_missing_or_invalid_number_exits_2() {
    // Each case is missing one number or carries a value `u64` cannot parse.
    let cases: &[&[&str]] = &[
        // Missing numbers.
        &["--plan"],
        &["--plan", "--completed", "10", "--total", "100"],
        &["--plan", "--completed", "10", "--size", "10"],
        &["--plan", "--total", "100", "--size", "10"],
        // Invalid numbers.
        &[
            "--plan",
            "--completed",
            "abc",
            "--total",
            "100",
            "--size",
            "10",
        ],
        &[
            "--plan",
            "--completed",
            "10",
            "--total",
            "xyz",
            "--size",
            "10",
        ],
        &[
            "--plan",
            "--completed",
            "10",
            "--total",
            "100",
            "--size",
            "abc",
        ],
        &[
            "--plan",
            "--completed",
            "-1",
            "--total",
            "100",
            "--size",
            "10",
        ],
        &[
            "--plan",
            "--completed",
            "10",
            "--total",
            "100",
            "--size",
            "12x",
        ],
        &[
            "--plan",
            "--completed",
            "",
            "--total",
            "100",
            "--size",
            "10",
        ],
    ];
    for (i, args) in cases.iter().enumerate() {
        let output = run_raw(args);
        assert_exit_2(&output, &format!("plan number case {i}: {args:?}"));
    }
}

#[test]
fn plan_size_zero_exits_2() {
    for (completed, total) in [("0", "10"), ("5", "10")] {
        let args = [
            "--plan",
            "--completed",
            completed,
            "--total",
            total,
            "--size",
            "0",
        ];
        let output = run_raw(&args);
        assert_exit_2(&output, &format!("--size 0 ({completed}/{total})"));
    }
}

#[test]
fn validate_missing_flag_or_bad_table_exits_2() {
    let dir = scratch_dir("validate-flags");
    let input_path = dir.join("in.tsv");
    std::fs::write(&input_path, format!("{VALID_ARTIST}\n")).expect("write input");
    let valid_path = dir.join("valid.tsv");
    let poison_path = dir.join("poison.tsv");
    let base = validate_args("artist", &input_path, &valid_path, &poison_path, "0");
    // Drop each required flag in turn, plus an unknown table value. Argv
    // positions: --table v, --in v, --valid-out v, --poison-out v,
    // --line-offset v (10 entries).
    let mut cases: Vec<Vec<String>> = Vec::new();
    for drop_flag in [
        "--table",
        "--in",
        "--valid-out",
        "--poison-out",
        "--line-offset",
    ] {
        let mut argv = Vec::new();
        let mut skip = false;
        for chunk in base.chunks(2) {
            if chunk[0] == drop_flag {
                skip = true;
                continue;
            }
            argv.push(chunk[0].clone());
            argv.push(chunk[1].clone());
        }
        assert!(skip, "flag {drop_flag} must exist in base argv");
        cases.push(argv);
    }
    // Unknown table value.
    let mut bad_table = base.clone();
    bad_table[1] = "bogus".to_string();
    cases.push(bad_table);

    for (i, argv) in cases.iter().enumerate() {
        let output = run_validate_argv(argv.clone());
        assert_exit_2(&output, &format!("validate flag case {i}: {argv:?}"));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn validate_missing_or_invalid_line_offset_exits_2() {
    let dir = scratch_dir("line-offset");
    let input_path = dir.join("in.tsv");
    std::fs::write(&input_path, format!("{VALID_ARTIST}\n")).expect("write input");
    for (i, offset) in ["abc", "-3", "12x", ""].iter().enumerate() {
        let argv = validate_args(
            "artist",
            &input_path,
            &dir.join(format!("valid-{i}.tsv")),
            &dir.join(format!("poison-{i}.tsv")),
            offset,
        );
        let output = run_validate_argv(argv);
        assert_exit_2(&output, &format!("line-offset {offset:?}"));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn validate_missing_or_unreadable_input_exits_2() {
    let dir = scratch_dir("input");
    // Missing file: point --in at a path that was never created.
    let missing = dir.join("no-such-file.tsv");
    let argv = validate_args(
        "artist",
        &missing,
        &dir.join("valid.tsv"),
        &dir.join("poison.tsv"),
        "0",
    );
    assert_exit_2(&run_validate_argv(argv), "missing input file");
    // Unreadable input: a directory opens but `read_to_end` fails with
    // EISDIR, pinning the read-failure exit-2 site separately from open.
    let argv = validate_args(
        "artist",
        &dir,
        &dir.join("valid2.tsv"),
        &dir.join("poison2.tsv"),
        "0",
    );
    assert_exit_2(&run_validate_argv(argv), "directory as input");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn validate_output_create_failure_exits_2() {
    let dir = scratch_dir("create");
    let input_path = dir.join("in.tsv");
    std::fs::write(&input_path, format!("{VALID_ARTIST}\n")).expect("write input");
    // Parent directory does not exist: `File::create` fails with ENOENT.
    let no_parent_valid = dir.join("no-such-dir/valid.tsv");
    let argv = validate_args(
        "artist",
        &input_path,
        &no_parent_valid,
        &dir.join("poison.tsv"),
        "0",
    );
    assert_exit_2(&run_validate_argv(argv), "valid-out parent missing");
    let no_parent_poison = dir.join("no-such-dir/poison.tsv");
    let argv = validate_args(
        "artist",
        &input_path,
        &dir.join("valid2.tsv"),
        &no_parent_poison,
        "0",
    );
    assert_exit_2(&run_validate_argv(argv), "poison-out parent missing");
    // Output path is a directory: `File::create` fails with EISDIR.
    let argv = validate_args("artist", &input_path, &dir, &dir.join("poison3.tsv"), "0");
    assert_exit_2(&run_validate_argv(argv), "valid-out is directory");
    let argv = validate_args("artist", &input_path, &dir.join("valid4.tsv"), &dir, "0");
    assert_exit_2(&run_validate_argv(argv), "poison-out is directory");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn validate_row_or_flush_write_failure_exits_2() {
    let dev_full = std::path::Path::new("/dev/full");
    if !dev_full.exists() {
        eprintln!("SKIP: /dev/full missing on this platform");
        return;
    }
    let dir = scratch_dir("devfull");
    // Small valid input (< BufWriter capacity): the buffered write succeeds
    // and the explicit flush reports ENOSPC.
    let input_path = dir.join("in.tsv");
    std::fs::write(&input_path, format!("{VALID_ARTIST}\n")).expect("write input");
    let argv = validate_args(
        "artist",
        &input_path,
        dev_full,
        &dir.join("poison.tsv"),
        "0",
    );
    assert_exit_2(&run_validate_argv(argv), "valid flush to /dev/full");
    // Large valid input (> BufWriter capacity): an interior row write
    // flushes the buffer and reports ENOSPC on the row path.
    let mut big = Vec::new();
    for _ in 0..500 {
        big.extend_from_slice(VALID_ARTIST.as_bytes());
        big.push(b'\n');
    }
    let big_path = dir.join("big.tsv");
    std::fs::write(&big_path, &big).expect("write big input");
    let argv = validate_args(
        "artist",
        &big_path,
        dev_full,
        &dir.join("poison-big.tsv"),
        "0",
    );
    assert_exit_2(&run_validate_argv(argv), "valid row write to /dev/full");
    // Poison path: bad-uuid rows fail writing to a full poison file, both
    // for a single buffered row (flush) and a large multi-row input (row).
    let poison_in = dir.join("poison-in.tsv");
    std::fs::write(&poison_in, format!("{BAD_UUID_ARTIST}\n")).expect("write poison input");
    let argv = validate_args(
        "artist",
        &poison_in,
        &dir.join("valid-poison.tsv"),
        dev_full,
        "0",
    );
    assert_exit_2(&run_validate_argv(argv), "poison flush to /dev/full");
    let mut big_poison = Vec::new();
    for _ in 0..500 {
        big_poison.extend_from_slice(BAD_UUID_ARTIST.as_bytes());
        big_poison.push(b'\n');
    }
    let big_poison_path = dir.join("big-poison.tsv");
    std::fs::write(&big_poison_path, &big_poison).expect("write big poison input");
    let argv = validate_args(
        "artist",
        &big_poison_path,
        &dir.join("valid-poison-big.tsv"),
        dev_full,
        "0",
    );
    assert_exit_2(&run_validate_argv(argv), "poison row write to /dev/full");
    let _ = std::fs::remove_dir_all(&dir);
}
