//! Acceptance + property tests for the authoritative usage ledger.
//!
//! Written first (TDD RED): these fail until `nntune_api::ledger`
//! implements the module. Every test asserts real behavior against
//! in-memory intents; no network or database is contacted. The Hyperdrive
//! Worker entry reuses these pure rules plus the parameterized statements.

use nntune_api::ledger::{
    self, classify_fenced_replay, classify_replay, derive_play_idempotency_key, fold_monthly_totals,
    ledger_insert_statement, ledger_monthly_total_statement, ledger_select_by_key_statement,
    new_idempotency_key, period_month_for_timestamp, prepare_ledger_row, prepare_play_ledger_row,
    LedgerError, LedgerRow, ReplayOutcome,
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
    let long_user = "x".repeat(129);
    let long_track = "x".repeat(513);
    for (user, track, k) in [
        ("", "track-1", key.as_str()),
        ("user-alice", "", key.as_str()),
        ("", "", key.as_str()),
        (long_user.as_str(), "track-1", key.as_str()),
        ("user-alice", long_track.as_str(), key.as_str()),
    ] {
        assert_eq!(
            prepare_ledger_row(user, track, k, 48_000, 1, 1_767_225_600),
            Err(LedgerError::Invalid),
            "user len {} track len {}",
            user.len(),
            track.len()
        );
    }
    // Handles at exactly the cap pass (chars, not bytes). Track matches the
    // stream worker's 512 limit so a stream-accepted track is never rejected.
    let wide = "é".repeat(128);
    assert_eq!(wide.len(), 256);
    assert!(
        prepare_ledger_row(&wide, "track-1", &key, 48_000, 1, 1_767_225_600).is_ok(),
        "128 two-byte chars fit the 128-char user cap"
    );
    let wide_track = "t".repeat(512);
    assert!(
        prepare_ledger_row("user-alice", &wide_track, &key, 48_000, 1, 1_767_225_600).is_ok(),
        "512-char track fits the 512 cap"
    );
}

#[test]
fn prepare_rejects_bad_keys_zero_and_oversized_bytes() {
    let good_key = new_idempotency_key();
    // `:` is allowed: stream play keys are `play:{user}:{track}:{play}`.
    // Length cap is 1159 (longest derivable play key), alphabet is
    // `[A-Za-z0-9_:-]` on both Rust and SQL sides.
    for bad_key in [
        "",
        "!!!",
        "has space",
        "a/b",
        "a+b",
        "a=b",
        "play:has space:t:1",
        &"x".repeat(1160),
    ] {
        assert_eq!(
            prepare_ledger_row("user-alice", "track-1", bad_key, 48_000, 1, 1_767_225_600),
            Err(LedgerError::Invalid),
            "key {bad_key:?}"
        );
    }
    // A `play:` key that does not bind its identity fails closed, even with
    // an otherwise valid shape.
    assert_eq!(
        prepare_ledger_row("mallory", "t", "play:alice:t:1", 48_000, 1, 1_767_225_600),
        Err(LedgerError::Invalid),
        "forged play key must not bind"
    );
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
        insert.starts_with("INSERT INTO stream_play_ledger "),
        "right table (not the ADR daily usage_ledger): {insert}"
    );
    assert!(
        insert.contains("(idempotency_key, user_id, track_id, period_month, stream_seconds, bytes_served, recorded_at)"),
        "column order: {insert}"
    );
    assert!(
        insert.contains("SELECT $1, $2, $3, $4, $5, $6, $7 FROM"),
        "positional bind through the fence select: {insert}"
    );
    assert!(
        insert.contains("ON CONFLICT (idempotency_key) DO NOTHING"),
        "replay-safe: {insert}"
    );
    // ADR 0002 rule-3 writer fence in-statement (the library.rs genre
    // pattern): the play owner's users row gates the write, held FOR SHARE
    // through the insert. A bare VALUES insert would record plays behind the
    // account-deletion marker; removing the fence fails here.
    assert!(
        insert.contains(
            "FROM (SELECT id FROM users WHERE id = $2 AND deleting_at IS NULL FOR SHARE) AS writer_fence"
        ),
        "fenced through the play owner's users row: {insert}"
    );
    // A zero-row insert is ambiguous: the Worker selects the stored row and
    // classifies exact redelivery vs conflicting payload.
    let select = ledger_select_by_key_statement();
    assert!(
        select.contains("FROM stream_play_ledger WHERE idempotency_key = $1"),
        "select by key: {select}"
    );
    for sql in [insert, select, ledger_monthly_total_statement()] {
        assert!(!sql.contains('{'), "no interpolation: {sql}");
        assert!(!sql.contains("%s"), "no interpolation: {sql}");
    }
    let monthly = ledger_monthly_total_statement();
    assert!(
        monthly.contains("FROM stream_play_ledger WHERE user_id = $1 AND period_month = $2"),
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

#[test]
fn stream_play_feeds_the_ledger_without_silent_replays() {
    // Feed-through: the stream worker's CompletePlay identity
    // (user-1, track-9, play-7) derives `play:user-1:track-9:play-7` on both
    // sides; the ledger accepts it, converges an exact redelivery, and flags
    // a same-key payload disagreement as a conflict (never a silent rowcount
    // 0). Removing the `play:` acceptance, the identity binding, or the
    // select+classify flow fails this test.
    let first = prepare_play_ledger_row("user-1", "track-9", "play-7", 40_000, 1, 1_767_225_600)
        .expect("stream play feeds the ledger");
    assert_eq!(first.idempotency_key, "play:user-1:track-9:play-7");
    assert_eq!(
        derive_play_idempotency_key("user-1", "track-9", "play-7").unwrap(),
        first.idempotency_key,
        "api and stream derivations agree"
    );
    // Exact redelivery converges.
    let replay =
        prepare_play_ledger_row("user-1", "track-9", "play-7", 40_000, 1, 1_767_225_600).unwrap();
    assert_eq!(
        classify_replay(Some(&first), &replay),
        ReplayOutcome::Duplicate
    );
    // Same key, changed bytes: conflict, never converged.
    let mut tampered = first.clone();
    tampered.bytes_served = 41_500;
    tampered.stream_seconds = 41_500u64.div_ceil(ledger::STREAM_BYTES_PER_SEC);
    assert_eq!(
        classify_replay(Some(&first), &tampered),
        ReplayOutcome::Conflict
    );
    // Same key, changed beneficiary: conflict.
    let mut rebilled = first.clone();
    rebilled.user_id = "user-2".into();
    assert_eq!(
        classify_replay(Some(&first), &rebilled),
        ReplayOutcome::Conflict
    );
    // No stored row: fresh, never a duplicate.
    assert_eq!(classify_replay(None, &first), ReplayOutcome::Fresh);
    // A genuine second listen carries a different play id and bills its own
    // row instead of converging.
    let second =
        prepare_play_ledger_row("user-1", "track-9", "play-8", 40_000, 1, 1_767_225_600).unwrap();
    assert_ne!(second.idempotency_key, first.idempotency_key);
    assert_eq!(
        classify_replay(Some(&first), &second),
        ReplayOutcome::Conflict,
        "different play id with same bytes is a distinct window, not a replay"
    );
}

#[test]
fn migration_pins_rate_and_persisted_caps() {
    // The migration is the persisted half of the prepare gate: if its rate
    // literal drifts (e.g. 48000 -> 4800) every prepare-valid row fails at
    // the DB, or corrupt rows are accepted. Dropping the CHECK has the same
    // effect. This test reads the migration file so either mutant fails the
    // suite.
    const MIGRATION: &str = include_str!("../../../migrations/0020_stream_play_ledger.sql");
    let rate = ledger::STREAM_BYTES_PER_SEC.to_string();
    assert!(
        MIGRATION.contains(&format!("bytes_served / {rate}")),
        "migration must divide by STREAM_BYTES_PER_SEC ({rate})"
    );
    assert!(
        MIGRATION.contains(&format!("bytes_served % {rate}")),
        "migration must modulo by STREAM_BYTES_PER_SEC ({rate})"
    );
    // Literal pin: the rate re-bills history, so drift is deliberate only.
    assert_eq!(ledger::STREAM_BYTES_PER_SEC, 48_000);
    // Persisted caps mirror the prepare bounds: without them a row that
    // satisfies the rate CHECK can still exceed the per-row / time-domain
    // limits prepare enforces (LedgerRow fields alone do not enforce them).
    assert!(
        MIGRATION.contains(&format!(
            "stream_seconds <= {}",
            ledger::MAX_STREAM_SECONDS_PER_ROW
        )),
        "migration must cap stream_seconds at MAX_STREAM_SECONDS_PER_ROW"
    );
    assert!(
        MIGRATION.contains(&format!("bytes_served <= {}", ledger::MAX_BYTES_PER_ROW)),
        "migration must cap bytes_served at MAX_BYTES_PER_ROW"
    );
    assert!(
        MIGRATION.contains(&format!("recorded_at <= {}", ledger::MAX_RECORDED_AT)),
        "migration must cap recorded_at at MAX_RECORDED_AT"
    );
    assert_eq!(ledger::MAX_STREAM_SECONDS_PER_ROW, 86_400);
    assert_eq!(ledger::MAX_BYTES_PER_ROW, 4_147_200_000);
    assert_eq!(ledger::MAX_RECORDED_AT, 253_402_300_799);
    // Table identity: per-play windows live in `stream_play_ledger`, never
    // the ADR daily `usage_ledger` — a colliding name would let the future
    // daily CREATE silently no-op and then fail its (user_id, period)
    // upsert. Renaming the table without updating the statements fails here.
    assert!(
        MIGRATION.contains("CREATE TABLE IF NOT EXISTS stream_play_ledger ("),
        "migration must create stream_play_ledger"
    );
    assert!(
        !MIGRATION.contains("CREATE TABLE IF NOT EXISTS usage_ledger ("),
        "migration must not claim the ADR usage_ledger name"
    );
    // Rust/SQL alphabet and length agreement: play keys (`play:` + 128 +
    // 512 + 512 = 1159, `[A-Za-z0-9_:-]`) and 512-char tracks must persist;
    // shrinking either side rejects stream-accepted plays at the DB.
    assert!(
        MIGRATION.contains(&format!(
            "char_length(idempotency_key) BETWEEN 1 AND {}",
            ledger::MAX_IDEMPOTENCY_KEY_LEN
        )),
        "migration must persist the 1159-char key cap"
    );
    assert!(
        MIGRATION.contains("idempotency_key ~ '^[A-Za-z0-9_:-]+$'"),
        "migration must persist the key alphabet (including ':')"
    );
    assert!(
        MIGRATION.contains(&format!(
            "char_length(track_id) BETWEEN 1 AND {}",
            ledger::MAX_TRACK_ID_LEN
        )),
        "migration must persist the 512-char track cap"
    );
    assert_eq!(ledger::MAX_IDEMPOTENCY_KEY_LEN, 1_159);
    assert_eq!(ledger::MAX_TRACK_ID_LEN, 512);
}

#[test]
fn fenced_zero_row_insert_disambiguates_deletion_from_replay() {
    // Zero rows from the fenced DO NOTHING insert is three-ways ambiguous.
    // The Worker SELECTs the stored row and probes the fence
    // (ACCOUNT_DELETING_PROBE_SQL), then classifies: a stored row decides
    // without the probe (healthy duplicates never read as deleting), and
    // only a missing row consults the probe — fence closed fails closed
    // with AccountDeleting, fence open is Fresh.
    let first = prepare_play_ledger_row("user-1", "track-9", "play-7", 48_000, 1, 1_767_225_600)
        .expect("stream play feeds the ledger");
    let replay =
        prepare_play_ledger_row("user-1", "track-9", "play-7", 48_000, 1, 1_767_225_600).unwrap();
    assert_eq!(
        classify_fenced_replay(Some(&first), &replay, true),
        ReplayOutcome::Duplicate,
        "stored exact row acknowledges even with the marker set"
    );
    assert_eq!(
        classify_fenced_replay(Some(&first), &replay, false),
        ReplayOutcome::Duplicate
    );
    let mut tampered = first.clone();
    tampered.bytes_served = 96_000;
    tampered.stream_seconds = 2;
    assert_eq!(
        classify_fenced_replay(Some(&first), &tampered, false),
        ReplayOutcome::Conflict,
        "stored disagreement flags, never converges"
    );
    assert_eq!(
        classify_fenced_replay(None, &first, true),
        ReplayOutcome::AccountDeleting,
        "no stored row + closed fence refuses behind the marker"
    );
    assert_eq!(
        classify_fenced_replay(None, &first, false),
        ReplayOutcome::Fresh,
        "no stored row + open fence should have inserted"
    );
}
