//! Ledger property tests for the stream metering half.
//!
//! These tests exercise only the public [`nntune_stream::meter`] surface the
//! way the ledger builder consumes it: `served_bytes` totals per
//! (user, track) become ledger row inputs, and `stream_seconds_for_bytes`
//! must agree with every emitted event's `seconds`. Deterministic xorshift
//! streams stand in for a property-test RNG so this crate stays
//! dependency-free.

use nntune_stream::meter::{
    analytics_point, served_bytes, stream_seconds_for_bytes, MeterEvent, MeterKind,
    STREAM_BYTES_PER_SEC,
};

fn rng_next(state: &mut u64) -> u64 {
    *state ^= *state >> 12;
    *state ^= *state << 25;
    *state ^= *state >> 27;
    state.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

fn served_event(user: &str, track: &str, bytes: u64) -> MeterEvent {
    MeterEvent {
        kind: MeterKind::BytesServed,
        user_id: user.to_string(),
        track_id: track.to_string(),
        bytes,
        seconds: stream_seconds_for_bytes(bytes),
    }
}

#[test]
fn event_seconds_always_match_the_formula() {
    // Whatever the play path emits, a BytesServed event's seconds are the
    // ceiling of its bytes at the nominal rate: the fast signal and the
    // ledger row derived later from the same bytes cannot disagree.
    let mut state = 0x1E9u64;
    for i in 0usize..3_000 {
        let bytes = if i.is_multiple_of(5) {
            rng_next(&mut state) % (STREAM_BYTES_PER_SEC * 3 + 2)
        } else {
            rng_next(&mut state) % 8_000_000
        };
        let event = served_event("user-1", "track-9", bytes);
        assert_eq!(event.seconds, stream_seconds_for_bytes(event.bytes));
        assert_eq!(event.seconds, bytes.div_ceil(STREAM_BYTES_PER_SEC));
    }
}

#[test]
fn chunked_service_folds_to_the_whole_play() {
    // A track served as many small events (chunks, restarts, orphan flushes)
    // folds to the same byte total as one whole-track event: splitting never
    // creates or destroys billable bytes.
    let mut state = 777u64;
    for _ in 0..500 {
        let chunks = 1 + rng_next(&mut state) % 8;
        let mut events = Vec::new();
        let mut total = 0u64;
        for _ in 0..chunks {
            let bytes = 1 + rng_next(&mut state) % 100_000;
            total = total.saturating_add(bytes);
            events.push(served_event("user-1", "track-9", bytes));
        }
        assert_eq!(served_bytes(&events, "user-1", "track-9"), total);
        // And the whole-track event carries the same total.
        let whole = vec![served_event("user-1", "track-9", total)];
        assert_eq!(
            served_bytes(&whole, "user-1", "track-9"),
            served_bytes(&events, "user-1", "track-9")
        );
    }
}

#[test]
fn per_user_per_track_totals_are_independent() {
    // Folding is scoped: one user's bytes never leak into another user's or
    // another track's total, no matter the interleaving.
    let mut state = 31u64;
    for _ in 0..300 {
        let mut events = Vec::new();
        let mut expected_a = 0u64;
        let mut expected_b = 0u64;
        for _ in 0..10 {
            let bytes = 1 + rng_next(&mut state) % 50_000;
            if rng_next(&mut state).is_multiple_of(2) {
                events.push(served_event("alice", "track-a", bytes));
                expected_a += bytes;
            } else {
                events.push(served_event("bob", "track-a", bytes));
                expected_b += bytes;
            }
        }
        // Reverse the interleaving: folding is order-independent.
        events.reverse();
        assert_eq!(served_bytes(&events, "alice", "track-a"), expected_a);
        assert_eq!(served_bytes(&events, "bob", "track-a"), expected_b);
        assert_eq!(served_bytes(&events, "alice", "track-b"), 0);
    }
}

#[test]
fn analytics_batch_reconciles_with_folded_totals() {
    // Summing the Analytics Engine doubles over a batch reproduces the folded
    // byte total exactly, and the summed seconds stay within the split bound
    // (+1 per extra event) of the whole-play ceiling.
    let mut state = 0xB0Bu64;
    for _ in 0..300 {
        let chunks = 1 + rng_next(&mut state) % 6;
        let mut events = Vec::new();
        let mut total = 0u64;
        for _ in 0..chunks {
            let bytes = 1 + rng_next(&mut state) % 2_000_000;
            total += bytes;
            events.push(served_event("user-1", "track-9", bytes));
        }
        let mut bytes_sum = 0u64;
        let mut seconds_sum = 0u64;
        for event in &events {
            let point = analytics_point(event);
            assert_eq!(point.blobs[0], "bytes_served");
            bytes_sum += point.doubles[0] as u64;
            seconds_sum += point.doubles[1] as u64;
        }
        assert_eq!(bytes_sum, total);
        let whole = stream_seconds_for_bytes(total);
        assert!(seconds_sum >= whole, "chunks {chunks}");
        assert!(
            seconds_sum <= whole + chunks.saturating_sub(1),
            "chunks {chunks} total {total}"
        );
    }
}
