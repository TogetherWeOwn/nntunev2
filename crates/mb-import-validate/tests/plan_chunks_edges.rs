//! Zero-size hang-guard pins for `plan_chunks`.
//!
//! `plan_chunks` in `src/lib.rs` returns empty when `size == 0`, guarding the
//! `while start <= total` loop against a non-advancing underflow spin
//! (`start + size - 1` underflows for `size == 0`, and `end + 1` would never
//! advance past `start`). The in-file unit test only calls with size 2000,
//! so this suite pins the guard at both edges of the cursor plus a size-1
//! sanity check proving the guard is not over-broad.

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
