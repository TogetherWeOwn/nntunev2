//! `backup-drill`: synthetic restore drill + staging verify helper.
//!
//! Secrets-redacted by construction: stdout carries only the evidence
//! JSON (timestamps, counts, hashes). KEK material enters via
//! environment for `verify` and never appears in output. Exit 0 on
//! success, 2 on drill/verify failure, 1 on usage/IO errors.

use std::ffi::OsStr;
use std::io::BufReader;

use nntune_backup::bundle;
use nntune_backup::drill::{self, DrillError};
use nntune_backup::verify::{self, Keyring};
use zeroize::Zeroizing;

/// Default fixture size: 3 full 64 KiB `velmora.media.v1` chunks + a
/// partial chunk, so the drill always crosses chunk boundaries via ranged
/// reads.
const DEFAULT_FIXTURE_BYTES: usize = 3 * 65536 + 12345;
const MAX_FIXTURE_BYTES: usize = 16 * 1024 * 1024;
const KEK_PREFIX: &str = "BACKUP_DRILL_KEK_V";

fn usage() -> ! {
    eprintln!("usage:");
    eprintln!("  backup-drill synthetic [--bytes N (max {MAX_FIXTURE_BYTES})] [--tool NAME]");
    eprintln!(
        "  backup-drill verify --rows <jsonl>   # KEKs via BACKUP_DRILL_KEK_V<version> (hex)"
    );
    std::process::exit(1);
}

/// KEKs from the operator environment: `BACKUP_DRILL_KEK_V<version>`
/// holding 64 lowercase hex chars. Returned as a [`Keyring`] so the
/// material is zeroized on drop; callers drop it before any
/// `process::exit`, which would skip destructors. Values never reach
/// stdout.
fn keyring_from_env() -> Result<Keyring, &'static str> {
    let mut keys: std::collections::BTreeMap<u32, [u8; 32]> = std::collections::BTreeMap::new();
    // The process exits immediately after any error here, but KEK
    // bytes parsed before the bad variable must still not linger:
    // clear them before returning the error.
    for (name, value) in std::env::vars_os() {
        if !name.as_encoded_bytes().starts_with(KEK_PREFIX.as_bytes()) {
            continue;
        }
        let Some(version) = name
            .to_str()
            .and_then(|n| n[KEK_PREFIX.len()..].parse().ok())
        else {
            return clear_and_fail(&mut keys, "invalid KEK version");
        };
        if keys.contains_key(&version) {
            return clear_and_fail(&mut keys, "duplicate KEK version");
        }
        let Ok(value) = value.into_string().map(Zeroizing::new) else {
            return clear_and_fail(&mut keys, "invalid KEK value");
        };
        let value = value.trim();
        if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return clear_and_fail(&mut keys, "invalid KEK value");
        }
        let raw = Zeroizing::new(verify::decode_hex(value).expect("validated hex"));
        let mut arr = Zeroizing::new([0u8; 32]);
        arr.copy_from_slice(&raw);
        keys.insert(version, *arr);
    }
    if keys.is_empty() {
        return Err("no BACKUP_DRILL_KEK_V<version> in environment");
    }
    Ok(Keyring::new(keys))
}

/// Zeroize already-parsed KEK bytes, then report the parse failure.
/// Split out (rather than a closure) so the error sites can keep
/// borrowing `keys` mutably for subsequent inserts.
fn clear_and_fail(
    keys: &mut std::collections::BTreeMap<u32, [u8; 32]>,
    msg: &'static str,
) -> Result<Keyring, &'static str> {
    use zeroize::Zeroize;
    for k in keys.values_mut() {
        k.zeroize();
    }
    Err(msg)
}

fn main() {
    let mut args = std::env::args_os().skip(1);
    match args.next().as_deref().and_then(OsStr::to_str) {
        Some("synthetic") => {
            let mut bytes = None;
            let mut tool = None;
            while let Some(a) = args.next() {
                match a.to_str() {
                    Some("--bytes") if bytes.is_none() => {
                        bytes = Some(
                            args.next()
                                .and_then(|n| n.to_str()?.parse().ok())
                                .filter(|&n| n <= MAX_FIXTURE_BYTES)
                                .unwrap_or_else(|| usage()),
                        );
                    }
                    Some("--tool") if tool.is_none() => {
                        tool = Some(
                            args.next()
                                .and_then(|v| v.into_string().ok())
                                .filter(|v| !v.is_empty() && !v.starts_with("--"))
                                .unwrap_or_else(|| usage()),
                        );
                    }
                    _ => usage(),
                }
            }
            let plaintext: Vec<u8> = (0..bytes.unwrap_or(DEFAULT_FIXTURE_BYTES))
                .map(|i| (i % 251) as u8)
                .collect();
            match drill::run_synthetic_drill(&plaintext, tool.as_deref().unwrap_or("backup-drill"))
            {
                Ok((ev, bundle_dir, _)) => {
                    println!("{}", serde_json::to_string_pretty(&ev).unwrap());
                    // Temp dirs only: the drill proves restore from the
                    // bundle, it does not keep one.
                    if let Some(base) = bundle_dir.parent() {
                        let _ = std::fs::remove_dir_all(base);
                    }
                    if !ev.hash_match {
                        std::process::exit(2);
                    }
                }
                Err(DrillError::Bundle(bundle::BundleError::Io(_))) => {
                    eprintln!("error: io failure");
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(2);
                }
            }
        }
        Some("verify") => {
            if args.next().as_deref() != Some(OsStr::new("--rows")) {
                usage();
            }
            let rows = args.next().unwrap_or_else(|| usage());
            if args.next().is_some() {
                usage();
            }
            if !std::fs::symlink_metadata(&rows).is_ok_and(|m| m.is_file()) {
                rows_read_failure();
            }
            let file = std::fs::File::open(rows).unwrap_or_else(|_| rows_read_failure());
            let keyring = keyring_from_env().unwrap_or_else(|e| {
                eprintln!("error: {e}");
                std::process::exit(1);
            });
            let rep = verify::verify_reader(BufReader::new(file), &keyring, "backup-drill");
            // Zeroize before the fallible exit: `process::exit` skips
            // destructors, so dropping here is what clears the KEKs.
            drop(keyring);
            let rep = rep.unwrap_or_else(|_| rows_read_failure());
            println!("{}", serde_json::to_string_pretty(&rep).unwrap());
            if rep.failed > 0 || rep.rows == 0 {
                std::process::exit(2);
            }
        }
        _ => usage(),
    }
}

fn rows_read_failure() -> ! {
    eprintln!("error: invalid or unreadable rows export");
    std::process::exit(1);
}
