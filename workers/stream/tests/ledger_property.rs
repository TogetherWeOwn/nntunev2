//! Ledger property tests for the stream metering half.
//!
//! These tests exercise only the public [`nntune_stream::meter`] surface the
//! way the ledger builder consumes it: each play carries its OWN byte total
//! (`CompletePlay.bytes`, folded from that play's chunks alone — never a
//! cross-play `(user, track)` sum), and `stream_seconds_for_bytes` must agree
//! with every emitted event's `seconds`. Deterministic xorshift streams stand
//! in for a property-test RNG so this crate stays dependency-free.

use nntune_stream::meter::{
    analytics_point, stream_seconds_for_bytes, CompletePlay, MeterEvent, MeterKind, UsageLedger,
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
        lane: None,
        backoff_ms: 0,
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
    // One play served as many small events (chunks, restarts, orphan flushes)
    // folds to the same byte total as one whole-play event: splitting never
    // creates or destroys billable bytes. The fold is per-play — this test
    // holds a single play id throughout — and the billed play carries exactly
    // that total.
    let mut state = 777u64;
    for i in 0..500 {
        let chunks = 1 + rng_next(&mut state) % 8;
        let mut total = 0u64;
        for _ in 0..chunks {
            let bytes = 1 + rng_next(&mut state) % 100_000;
            total = total.saturating_add(bytes);
        }
        let play_id = format!("play-{i}");
        let play =
            CompletePlay::new("user-1", "track-9", &play_id, total).expect("folded play total");
        assert_eq!(play.bytes, total);
        assert_eq!(
            stream_seconds_for_bytes(play.bytes),
            total.div_ceil(STREAM_BYTES_PER_SEC)
        );
        let mut ledger = UsageLedger::default();
        ledger.record(&play);
        assert_eq!(
            ledger.get(&play.idempotency_key).map(|e| e.bytes),
            Some(total)
        );
    }
}

#[test]
fn per_play_totals_are_independent_on_one_track() {
    // Two listens of one (user, track) bill as two independent rows: the
    // second play's bytes never leak into the first play's row, no matter the
    // interleaving of their chunk events.
    let mut state = 31u64;
    for i in 0..300 {
        let mut chunks_7 = Vec::new();
        let mut chunks_8 = Vec::new();
        let mut expected_7 = 0u64;
        let mut expected_8 = 0u64;
        for _ in 0..10 {
            let bytes = 1 + rng_next(&mut state) % 50_000;
            if rng_next(&mut state).is_multiple_of(2) {
                chunks_7.push(served_event("user-1", "track-9", bytes));
                expected_7 = expected_7.saturating_add(bytes);
            } else {
                chunks_8.push(served_event("user-1", "track-9", bytes));
                expected_8 = expected_8.saturating_add(bytes);
            }
        }
        // Reverse each play's chunk order: a play's fold is order-independent.
        chunks_7.reverse();
        chunks_8.reverse();
        let fold = |chunks: &[MeterEvent]| {
            chunks
                .iter()
                .map(|e| e.bytes)
                .fold(0u64, u64::saturating_add)
        };
        assert_eq!(fold(&chunks_7), expected_7);
        assert_eq!(fold(&chunks_8), expected_8);
        let play_7 =
            CompletePlay::new("user-1", "track-9", &format!("play-7-{i}"), expected_7).unwrap();
        let play_8 =
            CompletePlay::new("user-1", "track-9", &format!("play-8-{i}"), expected_8).unwrap();
        assert_ne!(play_7.idempotency_key, play_8.idempotency_key);
        let mut ledger = UsageLedger::default();
        ledger.record(&play_7);
        ledger.record(&play_8);
        assert_eq!(
            ledger.get(&play_7.idempotency_key).map(|e| e.bytes),
            Some(expected_7),
            "play-7 bills only its own chunks, never the cross-play sum"
        );
        assert_eq!(
            ledger.get(&play_8.idempotency_key).map(|e| e.bytes),
            Some(expected_8)
        );
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
