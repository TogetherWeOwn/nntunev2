//! R2 multipart part-floor pins (tests-only).
//!
//! Contract under test (`workers/stream/src/cache.rs:249,263,271,280,292`):
//!
//! - `R2_MIN_PART == 5_242_880` (5 MiB): every part except the last must be
//!   at least this big, and all non-final parts must be the same size (R2
//!   errors 10011/10048 otherwise);
//! - `MultipartUpload::begin/part/complete/flushed_part_sizes` coalesce
//!   sub-floor segments into fixed `R2_MIN_PART`-byte parts.
//!
//! Boundaries pinned here:
//!
//! - feeding exactly `R2_MIN_PART` bytes flushes exactly one part of that
//!   size;
//! - feeding `R2_MIN_PART - 1` bytes flushes nothing;
//! - `complete()` returns all bytes in order (split feeds across the floor
//!   boundary reassemble exactly).
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - lowering/raising `R2_MIN_PART` breaks the literal pin and the exact
//!   boundary pair (one-under flushes nothing / exactly-at flushes one);
//! - flushing short of the floor (e.g. `>` instead of `>=`, or flushing
//!   partial parts) breaks the boundary pair;
//! - reordering or dropping bytes in `part`/`complete` breaks the
//!   in-order reassembly pin.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test -p nntune-stream --test r2_part_floor_acceptance
//! ```

use nntune_stream::cache::{MultipartUpload, R2_MIN_PART};

#[test]
fn r2_min_part_is_five_mib() {
    assert_eq!(
        R2_MIN_PART, 5_242_880,
        "R2 rejects non-final parts under 5 MiB (10011/10048); the floor must stay exactly 5 MiB"
    );
}

#[test]
fn exactly_floor_bytes_flush_exactly_one_full_part() {
    let mut up = MultipartUpload::begin("t1");
    up.part(&vec![0xABu8; R2_MIN_PART]);
    assert_eq!(
        up.flushed_part_sizes(),
        &[R2_MIN_PART],
        "exactly one floor-sized part must flush"
    );
    assert_eq!(up.bytes_staged(), R2_MIN_PART as u64);
}

#[test]
fn one_byte_under_floor_flushes_nothing() {
    let mut up = MultipartUpload::begin("t1");
    up.part(&vec![0xABu8; R2_MIN_PART - 1]);
    assert!(
        up.flushed_part_sizes().is_empty(),
        "undersized parts are rejected by R2; nothing below the floor may flush"
    );
    assert_eq!(up.bytes_staged(), (R2_MIN_PART - 1) as u64);
}

#[test]
fn complete_returns_all_bytes_in_order() {
    // Distinct halves fed across the floor boundary: the first half alone
    // flushes nothing, the second half trips exactly one full part, and the
    // tail rides along in `complete` in feed order.
    let mut up = MultipartUpload::begin("t1");
    let head = vec![0x11u8; R2_MIN_PART - 7];
    let tail = vec![0x22u8; 16];
    up.part(&head);
    assert!(
        up.flushed_part_sizes().is_empty(),
        "head alone is under the floor"
    );
    up.part(&tail);
    assert_eq!(
        up.flushed_part_sizes(),
        &[R2_MIN_PART],
        "head + tail crosses the floor once: one full part plus a remainder"
    );
    let full = up.complete();
    let mut expected = head.clone();
    expected.extend_from_slice(&tail);
    assert_eq!(
        full, expected,
        "complete must reassemble bytes in feed order"
    );
}
