use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use nntune_backup::{
    drill::{self, MIN_FIXTURE_BYTES},
    verify::VerifyReport,
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
        let _ = fs::remove_dir_all(&self.0);
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
fn cli_verify_refuses_oversized_invalid_or_symlinked_exports() {
    let scratch = Scratch::new("invalid-rows");
    let rows = scratch.0.join("rows.jsonl");
    let file = fs::File::create(&rows).unwrap();
    file.set_len(2 * MAX_CIPHERTEXT_BYTES as u64 + 32 * 1024)
        .unwrap();
    let error = b"error: invalid or unreadable rows export";
    assert_denied(verify_cli(&rows).output().unwrap(), error);
    fs::write(&rows, b"{}\n\xff").unwrap();
    assert_denied(verify_cli(&rows).output().unwrap(), error);
    #[cfg(unix)]
    {
        let link = scratch.0.join("link.jsonl");
        fs::write(&rows, b"{}\n").unwrap();
        std::os::unix::fs::symlink(&rows, &link).unwrap();
        assert_denied(verify_cli(&link).output().unwrap(), error);
    }
}

fn cli() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_backup-drill"));
    cmd.env_clear().env("BACKUP_DRILL_KEK_V7", "33".repeat(32));
    cmd
}

fn verify_cli(rows: &Path) -> Command {
    let mut cmd = cli();
    cmd.args(["verify", "--rows"]).arg(rows);
    cmd
}

fn assert_denied(output: Output, message: &[u8]) {
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.starts_with(message));
}

#[test]
fn cli_rejects_ambiguous_arguments_and_redacts_kek_errors() {
    for args in [
        &["synthetic", "--tool", "--bytes"][..],
        &["synthetic", "--bytes", "70001", "--bytes", "70002"],
        &["synthetic", "--bytes", "16777217"],
        &["verify", "--rows", "first", "--rows", "second"],
    ] {
        assert_denied(cli().args(args).output().unwrap(), b"usage:");
    }
    let scratch = Scratch::new("verify");
    let rows = scratch.0.join("rows.jsonl");
    fs::write(&rows, b" \n").unwrap();
    let mut verify = verify_cli(&rows);
    let output = verify.output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let report: VerifyReport = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!((report.rows, report.ok, report.failed), (1, 0, 1));

    let output = verify
        .env("BACKUP_DRILL_KEK_V07", "55".repeat(32))
        .output()
        .unwrap();
    assert_denied(output, b"error: duplicate KEK version\n");
    verify
        .env_remove("BACKUP_DRILL_KEK_V07")
        .env("BACKUP_DRILL_KEK_Vprivate", "fixture");
    let output = verify.output().unwrap();
    assert_denied(output, b"error: invalid KEK version\n");
}
