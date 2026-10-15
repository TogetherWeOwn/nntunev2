//! Corrupt-segment repair through the native container path.
//!
//! A synthetic recovery set (Main + FileDesc + IFSC, built in-test) guards a
//! small data file. One byte is corrupted, [`RecoverySet::verify`] rejects
//! the damage, the file is repaired via [`repair_with_binary`] — the same
//! native subprocess path the container batch job uses, with a fake provider
//! binary that restores the pristine bytes and prints the real binary's
//! terminal summary line — and the repaired file verifies byte-identical.
//! No network, no provider binary, no real Reed-Solomon: the fixture pins
//! the wiring (verify rejects, repair runs contained, bytes round-trip).
#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

use nntune_core::par2::repair::repair_with_binary;
use nntune_core::par2::RecoverySet;

const BLOCK_SIZE: u64 = 1024;
const FILE_LEN: usize = 8192;
const FILENAME: &str = "track.bin";

// Serializes repair tests within this binary: `repair_with_binary` forks,
// and a fork in one libtest thread can inherit another thread's still-open
// write fd, failing exec. One guard per fixture lifetime keeps staging and
// the fork from overlapping.
static SERIAL: Mutex<()> = Mutex::new(());
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct JobDir {
    root: PathBuf,
    scratch: PathBuf,
    binary: PathBuf,
    _serial: MutexGuard<'static, ()>,
}

impl JobDir {
    fn new(script: &str) -> Self {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let base = std::env::var_os("PAPERCLIP_RUN_SCRATCH_DIR")
            .or_else(|| std::env::var_os("PAPERCLIP_SCRATCH_DIR"))
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let mut sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = loop {
            let root = base.join(format!(
                "nntune-repair-verify-{}-{sequence}",
                std::process::id()
            ));
            match fs::DirBuilder::new().mode(0o700).create(&root) {
                Ok(()) => break root,
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    sequence = NEXT.fetch_add(1, Ordering::Relaxed);
                }
                Err(err) => panic!("create owned test directory: {err}"),
            }
        };
        let scratch = root.join("scratch");
        fs::create_dir(&scratch).unwrap();
        let binary = root.join("fake-par2.sh");
        fs::write(&binary, format!("#!/bin/sh\nset -eu\n{script}\n")).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            root,
            scratch,
            binary,
            _serial,
        }
    }
}

impl Drop for JobDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

// --- Synthetic recovery set (same packet rules as the par2 unit tests) ------

fn pristine_bytes() -> Vec<u8> {
    let mut x = 0x51ab_3c7d_29e1_04f5u64;
    let mut out = vec![0u8; FILE_LEN];
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
                h.update(&vec![0u8; BLOCK_SIZE as usize - block.len()]);
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

fn recovery_set_bytes(data: &[u8]) -> Vec<u8> {
    let whole_md5 = md5::compute(data).0;
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
    for crc in block_crcs(data) {
        ifsc_body.extend_from_slice(&[0u8; 16]);
        ifsc_body.extend_from_slice(&crc.to_le_bytes());
    }

    let mut set_bytes = Vec::new();
    set_bytes.extend(packet(&set_id, &ptype(b"FileDesc"), &fd_body));
    set_bytes.extend(packet(&set_id, &ptype(b"IFSC\0\0\0\0"), &ifsc_body));
    set_bytes.extend(packet(&set_id, &ptype(b"Main\0\0\0\0"), &main_body));
    set_bytes
}

#[test]
fn corrupt_segment_repairs_via_native_path_byte_identical() {
    let pristine = pristine_bytes();
    let set_bytes = recovery_set_bytes(&pristine);
    let set = RecoverySet::parse(&set_bytes).expect("synthetic set parses");
    set.verify(FILENAME, &pristine)
        .expect("pristine fixture verifies");

    // The fake provider binary emulates `par2 repair` at `-q` verbosity: it
    // restores the staged pristine bytes over the damaged target and prints
    // the real binary's terminal summary line. It runs with the scratch dir
    // as its working directory, exactly like the container batch job.
    let job = JobDir::new("cp pristine.bin track.bin\nprintf 'Repair complete.\\n'");
    fs::write(job.scratch.join("pristine.bin"), &pristine).unwrap();
    fs::write(job.scratch.join("set.par2"), &set_bytes).unwrap();

    // Corrupt one byte in the middle block: the segment-level damage the
    // ingest pipeline would hand to repair.
    let mut damaged = pristine.clone();
    damaged[FILE_LEN / 2] ^= 0x01;
    fs::write(job.scratch.join("track.bin"), &damaged).unwrap();
    set.verify(FILENAME, &damaged)
        .expect_err("corrupted fixture must fail verification");

    let report = repair_with_binary(&job.binary, &job.scratch, &[Path::new("set.par2")])
        .expect("native repair path runs");
    assert!(report.success, "repair failed: {report:?}");
    assert_eq!(report.exit_code, Some(0));
    // At `-q` the real binary prints no block count: a repaired set reports
    // unknown (-1), never a spoofable zero.
    assert_eq!(report.repaired_blocks, -1);
    assert!(report.tail.contains("Repair complete."));

    let repaired = fs::read(job.scratch.join("track.bin")).unwrap();
    assert_eq!(
        repaired, pristine,
        "repaired bytes differ from the pristine fixture"
    );
    assert_eq!(md5::compute(&repaired).0, md5::compute(&pristine).0);
    set.verify(FILENAME, &repaired)
        .expect("repaired bytes verify against the recovery set");
}
