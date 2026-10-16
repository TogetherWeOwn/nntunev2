//! Acceptance + property tests for the authoritative usage ledger.
//!
//! Written first (TDD RED): these fail until `nntune_api::ledger`
//! implements the module. Every test asserts real behavior against
//! in-memory intents; no network or database is contacted. The Hyperdrive
//! Worker entry reuses these pure rules plus the parameterized statements.

use nntune_api::ledger::{
    self, fold_monthly_totals, ledger_insert_statement, ledger_monthly_total_statement,
    new_idempotency_key, period_month_for_timestamp, prepare_ledger_row, LedgerError, LedgerRow,
};

// --- Fixtures ---------------------------------------------------------------

fn row_for(user: &str, track: &str, bytes: u64, recorded_at: u64) -> LedgerRow {
    prepare_ledger_row(
        user,
        track,
        &new_idempotency_key(),
        bytes,
        bytes.div_ceil(ledger::STREAM_BYTES_PER_SEC),
        recorded_at,
    )
    .expect("fixture row")
}

// --- Prepare: happy path + rejections ---------------------------------------

#[test]
fn prepare_builds_a_fully_specified_intent() {
    let key = new_idempotency_key();
    let row = prepare_ledger_row(
        "user-alice",
        "track-opaque-1",
        &key,
        100_000,
        3,
        1_767_225_600,
    )
    .expect("valid row");
    assert_eq!(
        row,
        LedgerRow {
            idempotency_key: key,
            user_id: "user-alice".into(),
            track_id: "track-opaque-1".into(),
            period_month: "2026-01".into(),
            stream_seconds: 3,
            bytes_served: 100_000,
            recorded_at: 1_767_225_600,
        }
    );
}

#[test]
fn prepare_rejects_empty_and_overlong_handles() {
    let key = new_idempotency_key();
    let long = "x".repeat(129);
    for (user, track, k) in [
        ("", "track-1", key.as_str()),
        ("user-alice", "", key.as_str()),
        ("", "", key.as_str()),
        (long.as_str(), "track-1", key.as_str()),
        ("user-alice", long.as_str(), key.as_str()),
    ] {
        assert_eq!(
            prepare_ledger_row(user, track, k, 48_000, 1, 1_767_225_600),
            Err(LedgerError::Invalid),
            "user {user:?} track {track:?}"
        );
    }
    // Handles at exactly the cap pass (chars, not bytes).
    let wide = "é".repeat(128);
    assert_eq!(wide.len(), 256);
    assert!(
        prepare_ledger_row(&wide, "track-1", &key, 48_000, 1, 1_767_225_600).is_ok(),
        "128 two-byte chars fit the 128-char cap"
    );
}

#[test]
fn prepare_rejects_bad_keys_zero_and_oversized_bytes() {
    let good_key = new_idempotency_key();
    for bad_key in [
        "",
        "!!!",
        "has space",
        "a/b",
        "a+b",
        "a=b",
        &"x".repeat(129),
    ] {
        assert_eq!(
            prepare_ledger_row("user-alice", "track-1", bad_key, 48_000, 1, 1_767_225_600),
            Err(LedgerError::Invalid),
            "key {bad_key:?}"
        );
    }
    // Zero bytes bill nothing: rejected (the table CHECK agrees).
    assert_eq!(
        prepare_ledger_row("user-alice", "track-1", &good_key, 0, 0, 1_767_225_600),
        Err(LedgerError::Invalid)
    );
    // Past the per-row cap: corruption tripwire, rejected.
    assert_eq!(
        prepare_ledger_row(
            "user-alice",
            "track-1",
            &good_key,
            ledger::MAX_BYTES_PER_ROW + 1,
            ledger::MAX_STREAM_SECONDS_PER_ROW + 1,
            1_767_225_600,
        ),
        Err(LedgerError::Invalid)
    );
    // The cap itself is accepted with exactly 24 h attributed.
    let capped = prepare_ledger_row(
        "user-alice",
        "track-1",
        &good_key,
        ledger::MAX_BYTES_PER_ROW,
        ledger::MAX_STREAM_SECONDS_PER_ROW,
        1_767_225_600,
    )
    .expect("cap row");
    assert_eq!(
        (capped.bytes_served, capped.stream_seconds),
        (4_147_200_000, 86_400)
    );
}

#[test]
fn errors_are_generic_and_map_to_422() {
    let err = prepare_ledger_row("", "", "", 0, 0, 0).unwrap_err();
    assert_eq!(err, LedgerError::Invalid);
    assert_eq!(err.http_status(), 422);
    // Display never echoes the rejected handles.
    let rendered = format!("{err}");
    assert!(!rendered.contains("user-alice"));
}

// --- Period properties -------------------------------------------------------

fn rng_next(state: &mut u64) -> u64 {
    *state ^= *state >> 12;
    *state ^= *state << 25;
    *state ^= *state >> 27;
    state.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

fn period_shape_ok(period: &str) -> bool {
    let bytes = period.as_bytes();
    if bytes.len() != 7 || bytes[4] != b'-' {
        return false;
    }
    if !bytes[..4].iter().all(|b| b.is_ascii_digit()) {
        return false;
    }
    matches!(&bytes[5..], [b'0', b'1'..=b'9'] | [b'1', b'0'..=b'2'])
}

#[test]
fn periods_are_shaped_monotone_and_stable_within_a_month() {
    // Epoch month starts the range; every drawn timestamp yields YYYY-MM.
    assert_eq!(period_month_for_timestamp(0), "1970-01");
    let mut state = 0x70E4u64;
    for _ in 0..3_000 {
        // Mix second-granularity draws with month-boundary strata.
        let at = match rng_next(&mut state) % 4 {
            0 => 1_767_225_600 + (rng_next(&mut state) % 2) * 2_678_400,
            1 => 1_767_225_600 - 1 - rng_next(&mut state) % 100_000,
            _ => rng_next(&mut state) % 8_000_000_000,
        };
        let period = period_month_for_timestamp(at);
        assert!(period_shape_ok(&period), "shape of {period} at {at}");
    }
    // Non-decreasing over sorted time: months never run backwards.
    let mut times: Vec<u64> = {
        let mut s = 99u64;
        (0..2_000)
            .map(|_| rng_next(&mut s) % 8_000_000_000)
            .collect()
    };
    times.sort_unstable();
    let mut prev = String::new();
    for at in times {
        let period = period_month_for_timestamp(at);
        assert!(period >= prev, "monotone at {at}");
        prev = period;
    }
    // Every second of one month maps to the same period.
    let mut s2 = 7u64;
    for _ in 0..500 {
        let at = 1_767_225_600 + rng_next(&mut s2) % 2_678_400;
        assert_eq!(period_month_for_timestamp(at), "2026-01", "at {at}");
    }
}

// --- Fold properties ---------------------------------------------------------

#[test]
fn fold_groups_by_user_and_month_in_order() {
    let rows = vec![
        row_for("user-bob", "t1", 48_000, 1_767_225_600),
        row_for("user-alice", "t1", 96_000, 1_767_225_600),
        row_for("user-alice", "t2", 48_000, 1_709_164_800),
        row_for("user-alice", "t1", 48_001, 1_767_225_600),
    ];
    let totals = fold_monthly_totals(&rows);
    assert_eq!(totals.len(), 3);
    // Ordered by (user, month): alice/2024-02, alice/2026-01, bob/2026-01.
    assert_eq!(
        totals
            .iter()
            .map(|t| (t.user_id.as_str(), t.period_month.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("user-alice", "2024-02"),
            ("user-alice", "2026-01"),
            ("user-bob", "2026-01")
        ]
    );
    let alice_jan = &totals[1];
    assert_eq!(alice_jan.stream_seconds, 2 + 2);
    assert_eq!(alice_jan.bytes_served, 96_000 + 48_001);
    assert!(fold_monthly_totals(&[]).is_empty());
}

#[test]
fn fold_is_order_independent_and_split_preserving() {
    let mut state = 12345u64;
    for _ in 0..200 {
        let users = ["alice", "bob", "cara"];
        let mut rows = Vec::new();
        for _ in 0..12 {
            let user = users[(rng_next(&mut state) % 3) as usize];
            let bytes = 1 + rng_next(&mut state) % 3_000_000;
            let month_base = if rng_next(&mut state).is_multiple_of(2) {
                1_767_225_600
            } else {
                1_709_164_800
            };
            rows.push(row_for(user, "track-x", bytes, month_base));
        }
        let forward = fold_monthly_totals(&rows);
        rows.reverse();
        assert_eq!(fold_monthly_totals(&rows), forward, "order-independent");
        // Splitting one row's bytes across two rows preserves the group
        // total up to the one-second ceiling slack per extra row.
        if let Some(first) = rows.first().cloned() {
            let half = first.bytes_served / 2;
            let rest = first.bytes_served - half;
            let mut split = rows.clone();
            split.remove(0);
            let mk = |bytes: u64| {
                prepare_ledger_row(
                    &first.user_id,
                    &first.track_id,
                    &new_idempotency_key(),
                    bytes,
                    bytes.div_ceil(ledger::STREAM_BYTES_PER_SEC),
                    first.recorded_at,
                )
                .unwrap()
            };
            if half > 0 && rest > 0 {
                split.push(mk(half));
                split.push(mk(rest));
                let before: u64 = forward.iter().map(|t| t.bytes_served).sum();
                let after: u64 = fold_monthly_totals(&split)
                    .iter()
                    .map(|t| t.bytes_served)
                    .sum();
                assert_eq!(before, after, "bytes conserved across splits");
                let secs_before: u64 = forward.iter().map(|t| t.stream_seconds).sum();
                let secs_after: u64 = fold_monthly_totals(&split)
                    .iter()
                    .map(|t| t.stream_seconds)
                    .sum();
                assert!(secs_after >= secs_before);
                assert!(secs_after <= secs_before + 1);
            }
        }
    }
}

// --- SQL boundary: parameterized, replay-safe --------------------------------

#[test]
fn statements_are_parameterized_replay_safe_and_month_scoped() {
    let insert = ledger_insert_statement();
    assert!(
        insert.starts_with("INSERT INTO usage_ledger "),
        "right table: {insert}"
    );
    assert!(
        insert.contains("(idempotency_key, user_id, track_id, period_month, stream_seconds, bytes_served, recorded_at)"),
        "column order: {insert}"
    );
    assert!(
        insert.contains("VALUES ($1, $2, $3, $4, $5, $6, $7)"),
        "positional bind: {insert}"
    );
    assert!(
        insert.contains("ON CONFLICT (idempotency_key) DO NOTHING"),
        "replay-safe: {insert}"
    );
    for sql in [insert, ledger_monthly_total_statement()] {
        assert!(!sql.contains('{'), "no interpolation: {sql}");
        assert!(!sql.contains("%s"), "no interpolation: {sql}");
    }
    let monthly = ledger_monthly_total_statement();
    assert!(
        monthly.contains("FROM usage_ledger WHERE user_id = $1 AND period_month = $2"),
        "owner+period scope grouped as one AND unit: {monthly}"
    );
    assert!(
        monthly.contains("COALESCE(SUM(stream_seconds), 0)"),
        "zero, never NULL: {monthly}"
    );
    assert!(
        monthly.contains("COALESCE(SUM(bytes_served), 0)"),
        "zero, never NULL: {monthly}"
    );
    // Exactly one predicate group: no OR that could widen the scope.
    assert!(!monthly.contains(" OR "), "no scope widening: {monthly}");
}
