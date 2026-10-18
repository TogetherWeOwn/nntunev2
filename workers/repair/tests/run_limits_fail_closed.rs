//! Pin `run()` entry gate fail-closed: zero, over-ceiling, and empty inputs.
//!
//! `crates/nntune-core/src/par2/repair/linux.rs::run()` rejects a zero
//! timeout, any zero budget, any budget above the default ceiling, and an
//! empty `par2_files` list with `Error::Malformed` before
//! `canonicalize`/spawn/`prlimit`. These tests pin that gate with fake
//! executables only: a rejected call must return `Err` and must not have
//! spawned the fake binary (no `invoked` artifact in the scratch dir).
#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use nntune_core::par2::repair::{repair_with_limits, RepairLimits};

static NEXT: AtomicUsize = AtomicUsize::new(0);

// Serializes fork-based tests within this test binary; see
// `crates/nntune-core/tests/review_support/repair.rs` for why a fork in one
// libtest thread can otherwise inherit another thread's open script fd.
static SERIAL: Mutex<()> = Mutex::new(());

struct Fixture {
    scratch: PathBuf,
    root: PathBuf,
    binary: PathBuf,
    _serial: MutexGuard<'static, ()>,
}

impl Fixture {
    fn new(script: &str) -> Self {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let base = std::env::var_os("PAPERCLIP_RUN_SCRATCH_DIR")
            .or_else(|| std::env::var_os("PAPERCLIP_SCRATCH_DIR"))
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let mut sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = loop {
            let root = base.join(format!(
                "nntune-run-limits-test-{}-{sequence}",
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
        let scratch = root.join("job space");
        fs::create_dir(&scratch).unwrap();
        let binary = root.join("fake-par2.sh");
        fs::write(&binary, format!("#!/bin/sh\nset -eu\n{script}\n")).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(scratch.join("set.par2"), b"fake binary fixture\n").unwrap();
        Self {
            scratch,
            root,
            binary,
            _serial,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Otherwise-valid budgets, comfortably inside the default ceilings
/// (120 s, 1 MiB output, 8 GiB workspace, 512 MiB memory, 60 s CPU).
fn valid_limits() -> RepairLimits {
    RepairLimits {
        timeout: Duration::from_secs(2),
        max_output_bytes: 1024,
        max_workspace_bytes: 1024 * 1024,
        max_memory_bytes: 128 * 1024 * 1024,
        cpu_seconds: 1,
    }
}

fn assert_gate_rejects(
    result: nntune_core::error::Result<nntune_core::par2::repair::RepairReport>,
    context: &str,
    fixture: &Fixture,
) {
    match result {
        Err(err) => assert!(
            err.to_string()
                .contains("positive budgets within default ceilings"),
            "{context}: wrong gate message: {err}"
        ),
        Ok(report) => panic!("{context}: gate accepted, got {report:?}"),
    }
    assert!(
        !fixture.scratch.join("invoked").exists(),
        "{context}: fake binary spawned despite gate rejection"
    );
}

const SPAWN_SENTINEL: &str = "printf 'invoked' > invoked; printf 'Repair complete.\\n'";

#[test]
fn zero_timeout_is_rejected_before_spawn() {
    let f = Fixture::new(SPAWN_SENTINEL);
    let limits = RepairLimits {
        timeout: Duration::ZERO,
        ..valid_limits()
    };
    let result = repair_with_limits(
        &f.binary,
        &f.scratch,
        &[f.scratch.join("set.par2").as_path()],
        &limits,
    );
    // Absolute path inside scratch exercises the same gate first.
    assert_gate_rejects(result, "zero timeout", &f);
}

#[test]
fn zero_max_output_bytes_is_rejected_before_spawn() {
    let f = Fixture::new(SPAWN_SENTINEL);
    let limits = RepairLimits {
        max_output_bytes: 0,
        ..valid_limits()
    };
    let result = repair_with_limits(
        &f.binary,
        &f.scratch,
        &[f.scratch.join("set.par2").as_path()],
        &limits,
    );
    assert_gate_rejects(result, "zero max_output_bytes", &f);
}

#[test]
fn zero_max_workspace_bytes_is_rejected_before_spawn() {
    let f = Fixture::new(SPAWN_SENTINEL);
    let limits = RepairLimits {
        max_workspace_bytes: 0,
        ..valid_limits()
    };
    let result = repair_with_limits(
        &f.binary,
        &f.scratch,
        &[f.scratch.join("set.par2").as_path()],
        &limits,
    );
    assert_gate_rejects(result, "zero max_workspace_bytes", &f);
}

#[test]
fn zero_max_memory_bytes_is_rejected_before_spawn() {
    let f = Fixture::new(SPAWN_SENTINEL);
    let limits = RepairLimits {
        max_memory_bytes: 0,
        ..valid_limits()
    };
    let result = repair_with_limits(
        &f.binary,
        &f.scratch,
        &[f.scratch.join("set.par2").as_path()],
        &limits,
    );
    assert_gate_rejects(result, "zero max_memory_bytes", &f);
}

#[test]
fn zero_cpu_seconds_is_rejected_before_spawn() {
    let f = Fixture::new(SPAWN_SENTINEL);
    let limits = RepairLimits {
        cpu_seconds: 0,
        ..valid_limits()
    };
    let result = repair_with_limits(
        &f.binary,
        &f.scratch,
        &[f.scratch.join("set.par2").as_path()],
        &limits,
    );
    assert_gate_rejects(result, "zero cpu_seconds", &f);
}

#[test]
fn over_ceiling_timeout_is_rejected_before_spawn() {
    let f = Fixture::new(SPAWN_SENTINEL);
    let max = RepairLimits::default();
    let limits = RepairLimits {
        timeout: max.timeout + Duration::from_secs(1),
        ..valid_limits()
    };
    let result = repair_with_limits(
        &f.binary,
        &f.scratch,
        &[f.scratch.join("set.par2").as_path()],
        &limits,
    );
    assert_gate_rejects(result, "over-ceiling timeout", &f);
}

#[test]
fn over_ceiling_max_output_bytes_is_rejected_before_spawn() {
    let f = Fixture::new(SPAWN_SENTINEL);
    let max = RepairLimits::default();
    let limits = RepairLimits {
        max_output_bytes: max.max_output_bytes + 1,
        ..valid_limits()
    };
    let result = repair_with_limits(
        &f.binary,
        &f.scratch,
        &[f.scratch.join("set.par2").as_path()],
        &limits,
    );
    assert_gate_rejects(result, "over-ceiling max_output_bytes", &f);
}

#[test]
fn over_ceiling_max_workspace_bytes_is_rejected_before_spawn() {
    let f = Fixture::new(SPAWN_SENTINEL);
    let max = RepairLimits::default();
    let limits = RepairLimits {
        max_workspace_bytes: max.max_workspace_bytes + 1,
        ..valid_limits()
    };
    let result = repair_with_limits(
        &f.binary,
        &f.scratch,
        &[f.scratch.join("set.par2").as_path()],
        &limits,
    );
    assert_gate_rejects(result, "over-ceiling max_workspace_bytes", &f);
}

#[test]
fn over_ceiling_max_memory_bytes_is_rejected_before_spawn() {
    let f = Fixture::new(SPAWN_SENTINEL);
    let max = RepairLimits::default();
    let limits = RepairLimits {
        max_memory_bytes: max.max_memory_bytes + 1,
        ..valid_limits()
    };
    let result = repair_with_limits(
        &f.binary,
        &f.scratch,
        &[f.scratch.join("set.par2").as_path()],
        &limits,
    );
    assert_gate_rejects(result, "over-ceiling max_memory_bytes", &f);
}

#[test]
fn over_ceiling_cpu_seconds_is_rejected_before_spawn() {
    let f = Fixture::new(SPAWN_SENTINEL);
    let max = RepairLimits::default();
    let limits = RepairLimits {
        cpu_seconds: max.cpu_seconds + 1,
        ..valid_limits()
    };
    let result = repair_with_limits(
        &f.binary,
        &f.scratch,
        &[f.scratch.join("set.par2").as_path()],
        &limits,
    );
    assert_gate_rejects(result, "over-ceiling cpu_seconds", &f);
}

#[test]
fn empty_inputs_are_rejected_before_spawn() {
    let f = Fixture::new(SPAWN_SENTINEL);
    let limits = valid_limits();
    let result = repair_with_limits(&f.binary, &f.scratch, &[], &limits);
    assert_gate_rejects(result, "empty par2_files", &f);
}
