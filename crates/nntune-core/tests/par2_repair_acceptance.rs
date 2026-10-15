//! Acceptance tests for [TOG-25180](/TOG/issues/TOG-25180) (roadmap R5 from
//! [TOG-25148](/TOG/issues/TOG-25148)): corrupt-segment repair vector.
//!
//! Builder contract (implement in
//! `crates/nntune-core/src/par2/repair_verify.rs`, exposed as
//! `pub mod repair_verify` in `crates/nntune-core/src/par2.rs`):
//!
//! ```rust,ignore
//! use std::path::Path;
//! use crate::par2::RecoverySet;
//!
//! pub struct RepairVerifyReport {
//!     /// True when the repair binary ran and the target changed.
//!     pub repaired: bool,
//!     /// True when post-run `verify` passes (always true on `Ok`).
//!     pub verified: bool,
//! }
//!
//! pub fn repair_and_verify(
//!     par2_bin: &Path,
//!     scratch_dir: &Path,
//!     target: &Path,        // relative path of the damaged file in scratch_dir
//!     par2_files: &[&Path], // relative par2 paths in scratch_dir
//!     set: &RecoverySet,    // parsed recovery set to verify against
//!     filename: &str,       // protected file name inside the set
//! ) -> crate::error::Result<RepairVerifyReport>;
//! ```
//!
//! Semantics the Builder must implement:
//! - When `set.verify(filename, &current_bytes)` already passes, return
//!   `{ repaired: false, verified: true }` WITHOUT spawning the binary.
//! - Otherwise run the native repair path (`repair_with_binary` containment),
//!   re-read the target, `verify` again; on success return
//!   `{ repaired: true, verified: true }`, else return the verify error.
//! - Byte-identity is proven by THESE tests comparing file bytes to the
//!   pristine fixture, not by the report alone.
//!
//! Failing command:
//! `cargo test -p nntune-core --test par2_repair_acceptance`
//!
//! RED reason (2026-10-15): `nntune_core::par2::repair_verify` does not exist
//! yet, so this target fails to compile with `unresolved import` (E0432).
//! The Builder on [TOG-25180](/TOG/issues/TOG-25180) turns it green without
//! editing this file. Synthetic fixtures only, no provider.

#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use nntune_core::par2::repair_verify::repair_and_verify;
use nntune_core::par2::RecoverySet;

// --- Synthetic recovery set (minimal, self-contained) ------------------------

const BLOCK_SIZE: u64 = 256;
const FILENAME: &str = "track.flac";
const TARGET: &str = "track.flac";
const PAR2_NAME: &str = "set.par2";
const PRISTINE_NAME: &str = "pristine.bin";

fn file_bytes() -> Vec<u8> {
    let mut x = 0x51ab_3c7d_29e1_04f5u64;
    let mut out = vec![0u8; 1000];
    for b in out.iter_mut() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = (x >> 24) as u8;
    }
    out
}

fn block_crcs(data: &[u8]) -> Vec<u32> {
    data.chunks(BLOCK_SIZE as usize)
        .map(|block| {
            let mut h = crc32fast::Hasher::new();
            h.update(block);
            if block.len() < BLOCK_SIZE as usize {
                // PAR2 checksums the last slice zero-padded to full size.
                let pad = BLOCK_SIZE as usize - block.len();
                h.update(&vec![0u8; pad]);
            }
            h.finalize()
        })
        .collect()
}

fn packet(set_id: &[u8; 16], kind: &[u8; 16], body: &[u8]) -> Vec<u8> {
    let mut aligned = body.to_vec();
    while aligned.len() % 4 != 0 {
        aligned.push(0);
    }
    let mut p = Vec::new();
    p.extend_from_slice(b"PAR2\0PKT");
    p.extend_from_slice(&((64 + aligned.len()) as u64).to_le_bytes());
    p.extend_from_slice(&[0u8; 16]);
    p.extend_from_slice(set_id);
    p.extend_from_slice(kind);
    p.extend_from_slice(&aligned);
    let digest = md5::compute(&p[32..]).0;
    p[16..32].copy_from_slice(&digest);
    p
}

fn ptype(suffix: &[u8]) -> [u8; 16] {
    let mut t = [0u8; 16];
    t[..8].copy_from_slice(b"PAR 2.0\0");
    t[8..8 + suffix.len().min(8)].copy_from_slice(&suffix[..suffix.len().min(8)]);
    t
}

struct Fixture {
    data: Vec<u8>,
    set_bytes: Vec<u8>,
}

fn fixture() -> Fixture {
    let data = file_bytes();
    let crcs = block_crcs(&data);
    let whole_md5 = md5::compute(&data).0;
    let first16k = md5::compute(&data[..data.len().min(16 * 1024)]).0;
    let mut seed = Vec::new();
    seed.extend_from_slice(&first16k);
    seed.extend_from_slice(&(data.len() as u64).to_le_bytes());
    seed.extend_from_slice(FILENAME.as_bytes());
    let file_id = md5::compute(&seed).0;

    let mut main_body = Vec::new();
    main_body.extend_from_slice(&BLOCK_SIZE.to_le_bytes());
    main_body.extend_from_slice(&1u32.to_le_bytes());
    main_body.extend_from_slice(&file_id);
    let set_id = md5::compute(&main_body).0;

    let mut fd_body = Vec::new();
    fd_body.extend_from_slice(&file_id);
    fd_body.extend_from_slice(&whole_md5);
    fd_body.extend_from_slice(&first16k);
    fd_body.extend_from_slice(&(data.len() as u64).to_le_bytes());
    fd_body.extend_from_slice(FILENAME.as_bytes());
    while fd_body.len() % 4 != 0 {
        fd_body.push(0);
    }

    let mut ifsc_body = Vec::new();
    ifsc_body.extend_from_slice(&file_id);
    for crc in crcs {
        ifsc_body.extend_from_slice(&[0u8; 16]);
        ifsc_body.extend_from_slice(&crc.to_le_bytes());
    }

    let mut rs_body = Vec::new();
    rs_body.extend_from_slice(&7u32.to_le_bytes());

    let mut set_bytes = Vec::new();
    set_bytes.extend(packet(&set_id, &ptype(b"FileDesc"), &fd_body));
    set_bytes.extend(packet(&set_id, &ptype(b"RecvSlic"), &rs_body));
    set_bytes.extend(packet(&set_id, &ptype(b"IFSC\0\0\0\0"), &ifsc_body));
    set_bytes.extend(packet(&set_id, &ptype(b"Main\0\0\0\0"), &main_body));
    Fixture { data, set_bytes }
}

// --- Scratch staging ---------------------------------------------------------

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let base = std::env::var_os("PAPERCLIP_RUN_SCRATCH_DIR")
            .or_else(|| std::env::var_os("PAPERCLIP_SCRATCH_DIR"))
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let root = base.join(format!(
            "nntune-repair-accept-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    /// Writes `script` as an executable fake `par2` binary. The repair runner
    /// executes it with `current_dir == scratch`, so relative paths in the
    /// script resolve against staged files.
    fn fake_binary(&self, script: &str) -> PathBuf {
        let bin = self.root.join("fake-par2.sh");
        fs::write(&bin, format!("#!/bin/sh\nset -eu\n{script}\n")).unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
        bin
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        fs::write(self.root.join(name), bytes).unwrap();
    }

    fn read(&self, name: &str) -> Vec<u8> {
        fs::read(self.root.join(name)).unwrap()
    }

    fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

// --- Acceptance vectors --------------------------------------------------------

#[test]
fn corrupt_segment_repairs_to_byte_identical() {
    let f = fixture();
    let set = RecoverySet::parse(&f.set_bytes).expect("valid set parses");
    // Sanity: pristine verifies before we damage anything.
    set.verify(FILENAME, &f.data).expect("pristine verifies");

    let mut damaged = f.data.clone();
    damaged[500] ^= 0x01;
    assert!(
        set.verify(FILENAME, &damaged).is_err(),
        "damaged input must fail verify before repair"
    );

    let s = Scratch::new();
    s.write(TARGET, &damaged);
    s.write(PRISTINE_NAME, &f.data);
    s.write(PAR2_NAME, &f.set_bytes);
    // The fake binary simulates what the real par2 binary does: reconstruct
    // the target from recovery blocks. It restores the pristine bytes.
    let bin = s.fake_binary(&format!(
        "cp {PRISTINE_NAME} {TARGET}\nprintf 'Repair complete.\\n'"
    ));

    let report = repair_and_verify(
        &bin,
        s.path(),
        Path::new(TARGET),
        &[Path::new(PAR2_NAME)],
        &set,
        FILENAME,
    )
    .expect("repair of a single corrupt segment must succeed");

    assert!(report.repaired, "a damaged file must report repaired=true");
    assert!(report.verified, "a repaired file must report verified=true");
    assert_eq!(
        s.read(TARGET),
        f.data,
        "repaired bytes must be byte-identical to the pristine fixture"
    );
    set.verify(FILENAME, &s.read(TARGET))
        .expect("repaired bytes must verify against the recovery set");
}

#[test]
fn healthy_file_needs_no_repair_and_spawns_no_binary() {
    let f = fixture();
    let set = RecoverySet::parse(&f.set_bytes).expect("valid set parses");

    let s = Scratch::new();
    s.write(TARGET, &f.data);
    s.write(PAR2_NAME, &f.set_bytes);
    // If the implementation spawns the binary for a healthy file, this
    // script fails the run loudly (exit 99 + sentinel file).
    let bin = s.fake_binary("touch INVOKED_SENTINEL\nexit 99");

    let report = repair_and_verify(
        &bin,
        s.path(),
        Path::new(TARGET),
        &[Path::new(PAR2_NAME)],
        &set,
        FILENAME,
    )
    .expect("healthy file must verify without repair");

    assert!(
        !report.repaired,
        "a healthy file must report repaired=false (no binary needed)"
    );
    assert!(report.verified, "a healthy file must report verified=true");
    assert!(
        !s.path().join("INVOKED_SENTINEL").exists(),
        "repair binary must not run for a healthy file"
    );
    assert_eq!(s.read(TARGET), f.data, "healthy bytes must be untouched");
}

#[test]
fn unrecoverable_damage_reports_error_never_false_success() {
    let f = fixture();
    let set = RecoverySet::parse(&f.set_bytes).expect("valid set parses");

    let mut damaged = f.data.clone();
    damaged[100] ^= 0xff;
    damaged[500] ^= 0x01;

    let s = Scratch::new();
    s.write(TARGET, &damaged);
    s.write(PAR2_NAME, &f.set_bytes);
    // Simulates the real binary giving up: nonzero exit, target untouched.
    let bin = s.fake_binary("printf 'Repair failed.\\n'\nexit 2");

    let result = repair_and_verify(
        &bin,
        s.path(),
        Path::new(TARGET),
        &[Path::new(PAR2_NAME)],
        &set,
        FILENAME,
    );

    assert!(
        result.is_err(),
        "unrecoverable damage must surface as an error, got: {result:?}"
    );
    assert_eq!(
        s.read(TARGET),
        damaged,
        "a failed repair must leave the damaged bytes in place, never fake success"
    );
}
