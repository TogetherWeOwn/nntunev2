//! Pin `UsageLedger::total_bytes_for_user` saturating semantics.
//!
//! The fold at `workers/stream/src/meter.rs:370-376` uses `saturating_add`.
//! A wrapping fold would under-bill usage (e.g. `MAX - 1 + 2` wraps to `1`);
//! a panicking fold would break the reconcile path. These pins use
//! `u64::MAX`-adjacent values — the existing ledger/meter suites only use
//! small values — so a saturating-to-wrapping regression fails loudly.

use nntune_stream::meter::{CompletePlay, UsageLedger};

fn play(user: &str, track: &str, play_id: &str, bytes: u64) -> CompletePlay {
    CompletePlay::new(user, track, play_id, bytes).expect("valid play identity")
}

#[test]
fn max_minus_one_plus_two_saturates_at_max() {
    let mut ledger = UsageLedger::default();
    ledger.record(&play("user-1", "track-9", "play-a", u64::MAX - 1));
    ledger.record(&play("user-1", "track-9", "play-b", 2));
    assert_eq!(
        ledger.total_bytes_for_user("user-1"),
        u64::MAX,
        "MAX-1 + 2 must saturate at MAX, not wrap to 1"
    );
}

#[test]
fn saturation_is_per_user_independent() {
    let mut ledger = UsageLedger::default();
    ledger.record(&play("user-1", "track-9", "play-a", u64::MAX - 1));
    ledger.record(&play("user-1", "track-9", "play-b", 2));
    ledger.record(&play("user-2", "track-9", "play-c", 123));
    assert_eq!(ledger.total_bytes_for_user("user-1"), u64::MAX);
    assert_eq!(
        ledger.total_bytes_for_user("user-2"),
        123,
        "one user's saturation must not leak into another user's total"
    );
    assert_eq!(ledger.total_bytes_for_user("user-unknown"), 0);
}

#[test]
fn max_plus_max_is_order_independent() {
    let first = play("user-1", "track-9", "play-a", u64::MAX);
    let second = play("user-1", "track-9", "play-b", u64::MAX);
    let mut forward = UsageLedger::default();
    forward.record(&first);
    forward.record(&second);
    let mut reversed = UsageLedger::default();
    reversed.record(&second);
    reversed.record(&first);
    assert_eq!(forward.total_bytes_for_user("user-1"), u64::MAX);
    assert_eq!(reversed.total_bytes_for_user("user-1"), u64::MAX);
    assert_eq!(
        forward.total_bytes_for_user("user-1"),
        reversed.total_bytes_for_user("user-1"),
        "[MAX, MAX] total must not depend on record order"
    );
}
