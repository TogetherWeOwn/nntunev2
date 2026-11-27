use std::fs;
use std::io::{self, BufReader, Read};
use std::path::PathBuf;
use std::process::{Command, Output};

use nntune_backup::{
    drill::{self, MIN_FIXTURE_BYTES},
    verify::{self, Keyring, VerifyReport},
};
use nntune_core::original_source::MAX_CIPHERTEXT_BYTES;

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("backup-{}-{label}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn restore_requires_fresh_separate_destination() {
    let scratch = Scratch::new("restore");
    let source = scratch.0.join("backup");
    let (_, keyring) =
        drill::backup_fixture(&source, &vec![1; MIN_FIXTURE_BYTES as usize], "test").unwrap();
    let existing = scratch.0.join("existing");
    fs::create_dir(&existing).unwrap();
    let nested = source.join("nested");
    for target in [&existing, &nested, &source] {
        assert!(drill::restore_and_decrypt(&source, target, &keyring, "test").is_err());
    }
    assert!(!nested.exists());
    assert_eq!(fs::read_dir(existing).unwrap().count(), 0);
    let restored = scratch.0.join("fresh");
    assert!(drill::restore_and_decrypt(&source, &restored, &keyring, "test").is_ok());
}

#[test]
fn streaming_verify_rejects_oversized_or_unreadable_rows() {
    let keyring = Keyring::default();
    let overlong = io::repeat(b' ').take(2 * MAX_CIPHERTEXT_BYTES as u64 + 32 * 1024);
    let input = BufReader::new(b"{}\n".as_slice().chain(overlong));
    assert_eq!(
        verify::verify_reader(input, &keyring, "test")
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert!(verify::verify_reader(&b"{}\n\xff\n"[..], &keyring, "test").is_err());
}

fn cli() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_backup-drill"));
    cmd.env_clear().env("BACKUP_DRILL_KEK_V7", "33".repeat(32));
    cmd
}

fn assert_usage(output: Output) {
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
}

#[test]
fn cli_rejects_ambiguous_arguments_and_redacts_kek_errors() {
    for args in [
        &["synthetic", "--tool", "--bytes"][..],
        &["synthetic", "--bytes", "70001", "--bytes", "70002"],
        &["synthetic", "--bytes", "16777217"],
        &["verify", "--rows", "first", "--rows", "second"],
    ] {
        assert_usage(cli().args(args).output().unwrap());
    }
    let scratch = Scratch::new("verify");
    let rows = scratch.0.join("rows.jsonl");
    fs::write(&rows, b" \n").unwrap();
    let mut verify = cli();
    verify.arg("verify").arg("--rows").arg(rows);
    let output = verify.output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let report: VerifyReport = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!((report.rows, report.ok, report.failed), (1, 0, 1));

    let output = verify
        .env("BACKUP_DRILL_KEK_V07", "55".repeat(32))
        .output()
        .unwrap();
    assert_usage(output);
    verify
        .env_remove("BACKUP_DRILL_KEK_V07")
        .env("BACKUP_DRILL_KEK_Vprivate", "fixture");
    let output = verify.output().unwrap();
    assert_eq!(output.stderr, b"error: invalid KEK version\n");
    assert_usage(output);
}
