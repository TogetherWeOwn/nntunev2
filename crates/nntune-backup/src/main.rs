//! `backup-drill`: synthetic restore drill + staging verify helper.
//!
//! Secrets-redacted by construction: stdout carries only the evidence
//! JSON (timestamps, counts, hashes). KEK material enters via
//! environment for `verify` and never appears in output. Exit 0 on
//! success, 2 on drill/verify failure, 1 on usage/IO errors.

use nntune_backup::bundle;
use nntune_backup::drill::{self, DrillError};
use nntune_backup::verify::{self, Keyring};

/// Default fixture size: 3 full 64 KiB rclone blocks + a partial block,
/// so the drill always crosses block boundaries via ranged reads.
const DEFAULT_FIXTURE_BYTES: usize = 3 * 65536 + 12345;

fn usage() -> ! {
    eprintln!("usage:");
    eprintln!("  backup-drill synthetic [--bytes N] [--tool NAME]");
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
fn keyring_from_env() -> Result<Keyring, String> {
    let mut keys: std::collections::BTreeMap<u32, [u8; 32]> = std::collections::BTreeMap::new();
    for (name, value) in std::env::vars() {
        if let Some(v) = name.strip_prefix("BACKUP_DRILL_KEK_V") {
            let version: u32 = v.parse().map_err(|_| format!("bad KEK var: {name}"))?;
            let raw = verify::decode_hex(value.trim()).ok_or_else(|| format!("{name}: bad hex"))?;
            if raw.len() != 32 {
                return Err(format!("{name}: want 64 hex chars"));
            }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&raw);
            keys.insert(version, arr);
        }
    }
    if keys.is_empty() {
        return Err("no BACKUP_DRILL_KEK_V<version> in environment".to_string());
    }
    Ok(Keyring::new(keys))
}

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("synthetic") => {
            let mut bytes = DEFAULT_FIXTURE_BYTES;
            let mut tool = "backup-drill".to_string();
            while let Some(a) = args.next() {
                match a.as_str() {
                    "--bytes" => {
                        bytes = args
                            .next()
                            .unwrap_or_else(|| usage())
                            .parse()
                            .unwrap_or_else(|_| usage());
                    }
                    "--tool" => {
                        tool = args.next().unwrap_or_else(|| usage());
                    }
                    _ => usage(),
                }
            }
            let plaintext: Vec<u8> = (0..bytes).map(|i| (i % 251) as u8).collect();
            match drill::run_synthetic_drill(&plaintext, &tool) {
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
            let mut rows: Option<String> = None;
            while let Some(a) = args.next() {
                match a.as_str() {
                    "--rows" => rows = args.next(),
                    _ => usage(),
                }
            }
            let Some(rows) = rows else { usage() };
            let raw = std::fs::read(&rows).unwrap_or_else(|e| {
                eprintln!("error: rows read: {e}");
                std::process::exit(1);
            });
            let text = String::from_utf8(raw).unwrap_or_else(|_| {
                eprintln!("error: rows not UTF-8");
                std::process::exit(1);
            });
            let keyring = keyring_from_env().unwrap_or_else(|e| {
                eprintln!("error: {e}");
                std::process::exit(1);
            });
            let rep = verify::verify_rows(&text, &keyring, "backup-drill");
            println!("{}", serde_json::to_string_pretty(&rep).unwrap());
            // Zeroize before the fallible exit: `process::exit` skips
            // destructors, so dropping here is what clears the KEKs.
            drop(keyring);
            if rep.failed > 0 || rep.rows == 0 {
                std::process::exit(2);
            }
        }
        _ => usage(),
    }
}
