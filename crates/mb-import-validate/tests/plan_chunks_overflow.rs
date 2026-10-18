//! Boundary regressions with at most two output ranges per call.

use mb_import_validate::plan_chunks;

#[test]
fn maximum_size_clamps_to_a_single_line() {
    assert_eq!(plan_chunks(0, 1, u64::MAX), vec![(1, 1)]);
}

#[test]
fn maximum_size_clamps_near_the_integer_limit() {
    assert_eq!(
        plan_chunks(u64::MAX - 2, u64::MAX, u64::MAX),
        vec![(u64::MAX - 1, u64::MAX)]
    );
}

#[test]
fn final_single_line_at_maximum_does_not_increment_again() {
    assert_eq!(
        plan_chunks(u64::MAX - 1, u64::MAX, 1),
        vec![(u64::MAX, u64::MAX)]
    );
}

#[test]
fn two_single_line_chunks_reach_the_integer_limit() {
    assert_eq!(
        plan_chunks(u64::MAX - 2, u64::MAX, 1),
        vec![(u64::MAX - 1, u64::MAX - 1), (u64::MAX, u64::MAX)]
    );
}

#[test]
fn ordinary_partial_final_chunk_stays_contiguous() {
    assert_eq!(plan_chunks(3, 8, 3), vec![(4, 6), (7, 8)]);
}

#[test]
fn exact_final_chunk_stops_at_total() {
    assert_eq!(plan_chunks(0, 6, 3), vec![(1, 3), (4, 6)]);
}

#[test]
fn zero_size_and_completed_cursors_still_return_empty() {
    for (completed, total, size) in [
        (0, u64::MAX, 0),
        (u64::MAX - 1, u64::MAX, 0),
        (u64::MAX, u64::MAX, 1),
        (u64::MAX, u64::MAX - 1, 1),
        (0, 0, u64::MAX),
    ] {
        assert_eq!(plan_chunks(completed, total, size), Vec::new());
    }
}
