//! Gregorian-century month attribution pins for the usage ledger.
//!
//! The ledger attributes months with a dependency-free civil-date conversion
//! (Hinnant's algorithm with the 100/400-year correction). These vectors pin
//! the century exceptions the neighboring suites do not cover: 2000 and 2400
//! are leap years (February 29 exists), 2100 is not (February has 28 days).
//!
//! Timestamps below are literal UTC epoch seconds, independently verified
//! against the standard calendar — NOT calculated with `civil_from_days` or
//! the function under test. Generation command (Python stdlib only):
//!
//! ```sh
//! python3 -c "import datetime;
//! cases = ['2000-02-29T00:00:00Z','2000-02-29T23:59:59Z','2000-03-01T00:00:00Z','2100-02-28T00:00:00Z','2100-02-28T23:59:59Z','2100-03-01T00:00:00Z','2400-02-29T00:00:00Z','2400-02-29T23:59:59Z','2400-03-01T00:00:00Z'];
//! [print(c, int(datetime.datetime.strptime(c, '%Y-%m-%dT%H:%M:%SZ').replace(tzinfo=datetime.timezone.utc).timestamp())) for c in cases]"
//! ```
//!
//! Cross-checked with `calendar.timegm` (same values) and `date -u -d` for
//! the leap-day existence question (`date -u -d 2000-02-29` succeeds,
//! `date -u -d 2100-02-29` is rejected as invalid).

use nntune_api::ledger::{
    fold_monthly_totals, period_month_for_timestamp, prepare_play_ledger_row,
};

// --- 2000 (leap century): February 29 exists ---------------------------------

#[test]
fn leap_century_2000_feb29_belongs_to_february() {
    // 2000-02-29T00:00:00Z.
    assert_eq!(period_month_for_timestamp(951_782_400), "2000-02");
    // 2000-02-29T23:59:59Z: the final second before March still bills February.
    assert_eq!(period_month_for_timestamp(951_868_799), "2000-02");
}

#[test]
fn leap_century_2000_march_boundary() {
    // Final second of February -> February; first second of March -> March.
    assert_eq!(period_month_for_timestamp(951_868_799), "2000-02");
    // 2000-03-01T00:00:00Z.
    assert_eq!(period_month_for_timestamp(951_868_800), "2000-03");
}

// --- 2400 (leap century): February 29 exists ---------------------------------

#[test]
fn leap_century_2400_feb29_belongs_to_february() {
    // 2400-02-29T00:00:00Z.
    assert_eq!(period_month_for_timestamp(13_574_563_200), "2400-02");
    // 2400-02-29T23:59:59Z: the final second before March still bills February.
    assert_eq!(period_month_for_timestamp(13_574_649_599), "2400-02");
}

#[test]
fn leap_century_2400_march_boundary() {
    // Final second of February -> February; first second of March -> March.
    assert_eq!(period_month_for_timestamp(13_574_649_599), "2400-02");
    // 2400-03-01T00:00:00Z.
    assert_eq!(period_month_for_timestamp(13_574_649_600), "2400-03");
}

// --- 2100 (non-leap century): February has 28 days ----------------------------

#[test]
fn non_leap_century_2100_february_march_boundary() {
    // 2100-02-28T00:00:00Z opens the last February day.
    assert_eq!(period_month_for_timestamp(4_107_456_000), "2100-02");
    // 2100-02-28T23:59:59Z: no Feb 29 exists, so this is the last February second.
    assert_eq!(period_month_for_timestamp(4_107_542_399), "2100-02");
    // 2100-03-01T00:00:00Z: the very next second is already March.
    assert_eq!(period_month_for_timestamp(4_107_542_400), "2100-03");
}

// --- Fold: February and March stay separate, totals preserved -----------------

#[test]
fn fold_separates_february_and_march_at_century_boundaries() {
    // One valid intent on each side of each boundary. Distinct play ids give
    // distinct `play:` keys; positive user handles and byte controls keep
    // every row prepare-valid (seconds = ceil(bytes / rate)).
    let feb_2000 = prepare_play_ledger_row(
        "user-century",
        "track-century",
        "play-2000-feb",
        48_000,
        1,
        951_868_799,
    )
    .expect("feb 2000 row");
    let mar_2000 = prepare_play_ledger_row(
        "user-century",
        "track-century",
        "play-2000-mar",
        96_000,
        2,
        951_868_800,
    )
    .expect("mar 2000 row");
    assert_eq!(feb_2000.period_month, "2000-02");
    assert_eq!(mar_2000.period_month, "2000-03");

    let feb_2100 = prepare_play_ledger_row(
        "user-century",
        "track-century",
        "play-2100-feb",
        48_000,
        1,
        4_107_542_399,
    )
    .expect("feb 2100 row");
    let mar_2100 = prepare_play_ledger_row(
        "user-century",
        "track-century",
        "play-2100-mar",
        96_000,
        2,
        4_107_542_400,
    )
    .expect("mar 2100 row");
    assert_eq!(feb_2100.period_month, "2100-02");
    assert_eq!(mar_2100.period_month, "2100-03");

    let feb_2400 = prepare_play_ledger_row(
        "user-century",
        "track-century",
        "play-2400-feb",
        48_000,
        1,
        13_574_649_599,
    )
    .expect("feb 2400 row");
    let mar_2400 = prepare_play_ledger_row(
        "user-century",
        "track-century",
        "play-2400-mar",
        96_000,
        2,
        13_574_649_600,
    )
    .expect("mar 2400 row");
    assert_eq!(feb_2400.period_month, "2400-02");
    assert_eq!(mar_2400.period_month, "2400-03");

    let rows = vec![feb_2000, mar_2000, feb_2100, mar_2100, feb_2400, mar_2400];
    let totals = fold_monthly_totals(&rows);
    assert_eq!(totals.len(), 6, "one bucket per century month: {totals:?}");
    for total in &totals {
        // One row per bucket here: exactly 1 s / 48_000 B in February buckets,
        // 2 s / 96_000 B in March buckets.
        if total.period_month.ends_with("-02") {
            assert_eq!((total.stream_seconds, total.bytes_served), (1, 48_000));
        } else {
            assert_eq!((total.stream_seconds, total.bytes_served), (2, 96_000));
        }
    }
    // Totals preserved across the fold.
    let bytes: u64 = totals.iter().map(|t| t.bytes_served).sum();
    let seconds: u64 = totals.iter().map(|t| t.stream_seconds).sum();
    assert_eq!(bytes, 48_000 * 3 + 96_000 * 3);
    assert_eq!(seconds, 1 * 3 + 2 * 3);
}
