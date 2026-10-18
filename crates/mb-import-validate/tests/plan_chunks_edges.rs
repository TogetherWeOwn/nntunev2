//! Zero-size hang-guard pins for `plan_chunks`.
//!
//! `plan_chunks` in `src/lib.rs` returns empty when `size == 0`, guarding the
//! `while start <= total` loop against a non-advancing underflow spin
//! (`start + size - 1` underflows for `size == 0`, and `end + 1` would never
//! advance past `start`). The in-file unit test only calls with size 2000,
//! so this suite pins the guard at both edges of the cursor plus a size-1
//! sanity check proving the guard is not over-broad.
//!
//! Test-safety note: removing the `size == 0` guard turns the two zero-size
//! tests below into a non-advancing push loop (the `Vec` grows without bound
//! and the test process is killed by the host memory limit, not by a clean
//! timeout). Never re-prove sensitivity with an unbounded in-process run.
//! The approved sensitivity method is: compile once with the normal build,
//! then run only the compiled test binary in an isolated child with an
//! explicit address-space ceiling (`ulimit -v 524288`, 512 MiB) and a
//! wall-clock timeout (`timeout 10s`). The `guard_removal_single_step_*`
//! test proves the same non-advancement mechanism in O(1) with no allocation
//! and no loop, so day-to-day confidence never needs the allocating mutant.

use mb_import_validate::plan_chunks;

#[test]
fn zero_size_returns_empty_from_start() {
    assert_eq!(plan_chunks(0, 5000, 0), Vec::new());
}

#[test]
fn zero_size_returns_empty_near_tail() {
    assert_eq!(plan_chunks(4999, 5000, 0), Vec::new());
}

#[test]
fn size_one_chunks_each_line() {
    assert_eq!(plan_chunks(0, 3, 1), vec![(1, 1), (2, 2), (3, 3)]);
}

/// Proves the hang mechanism with one loop-body step, no `Vec`, no loop.
///
/// This replays exactly one iteration of the unguarded `while start <= total`
/// body (`end = (start + size - 1).min(total); start = end + 1`) with
/// `size == 0` at both cursor edges and shows the cursor does not advance:
/// the next `start` equals the current one, so a guard-less loop would repeat
/// the identical state forever while pushing. It passes with O(1) time and
/// memory on any head, which is the point: it documents *why* the guard is
/// load-bearing without ever executing the allocating mutant.
#[test]
fn guard_removal_single_step_would_not_advance() {
    for (completed, total) in [(0u64, 5000u64), (4999u64, 5000u64)] {
        let size = 0u64;
        let start = completed + 1;
        // Same expression as the loop body in `plan_chunks`; safe here
        // because `start >= 1`, so `(start + 0) - 1` cannot underflow.
        let end = (start + size - 1).min(total);
        assert_eq!(
            end + 1,
            start,
            "unguarded body with size 0 does not advance (completed={completed}, total={total})"
        );
    }
}
