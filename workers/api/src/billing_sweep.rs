//! Inert selection of caller-supplied eligible subscription keys.
//!
//! This module does not query, schedule, persist, or dispatch work. The
//! caller must supply the complete eligible set and stable, unique keys (the
//! billing schema makes `stripe_subscription_id` unique). Keys and cursors
//! use Rust's bytewise string ordering, not a database collation.

pub const CRON: &str = "0 * * * *";
pub const BATCH_SIZE: i64 = 16;

/// Accept only the hourly trigger with a usable scheduled timestamp.
pub fn scheduled_hour(cron: &str, millis: f64) -> Option<i64> {
    (cron == CRON && millis.is_finite() && millis >= 0.0)
        .then(|| (millis / 3_600_000.0).floor() as i64)
}

/// One bounded, rotating window over the same due set. A persistent
/// failure stays eligible, but cannot hold a place at the front forever.
/// Parameters: UTC scheduled hour, positive batch size.
pub const DUE_TARGETS: &str = r#"
    WITH due AS (
        SELECT user_id, NULL::text AS subscription_id FROM billing_operations
        WHERE kind = 'checkout' AND state IN ('pending', 'submitted', 'needs_reconcile')
        UNION ALL
        SELECT user_id, stripe_subscription_id FROM subscriptions
        WHERE status IN ('incomplete', 'active', 'past_due', 'unpaid', 'paused')
          AND (paid_through_at IS NULL OR paid_through_at <= now())
    ), ordered AS (
        SELECT due.*, row_number() OVER (
            ORDER BY due.user_id COLLATE "C", subscription_id COLLATE "C" NULLS FIRST
        ) - 1 AS slot, count(*) OVER () AS eligible
        FROM due JOIN users ON users.id = due.user_id AND users.deleting_at IS NULL
    )
    SELECT user_id, subscription_id FROM ordered
    ORDER BY (slot - (($1 % eligible) * $2 % eligible) + eligible) % eligible
    LIMIT $2"#;

/// Duplicate keys are rejected rather than silently dropping eligible entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DuplicateKey;

/// Select at most `limit` keys in circular order strictly after `cursor`.
///
/// `None` starts at the smallest key. The returned cursor is the last key
/// selected, or the original cursor when the batch is empty. A cursor need
/// not be present in the set. Each key appears at most once per batch.
///
/// Repeated batches reach every key only for a stable eligibility set and
/// when the caller atomically persists and advances the returned cursor.
/// This function establishes no concurrency, scheduling, or delivery guarantee.
pub fn select<'a>(
    eligible: &[&'a str],
    cursor: Option<&'a str>,
    limit: usize,
) -> Result<(Vec<&'a str>, Option<&'a str>), DuplicateKey> {
    let mut keys = eligible.to_vec();
    keys.sort_unstable();
    if keys.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(DuplicateKey);
    }
    let start = cursor.map_or(0, |cursor| keys.partition_point(|key| *key <= cursor));
    let count = limit.min(keys.len());
    let selected: Vec<_> = keys[start..]
        .iter()
        .chain(keys[..start].iter())
        .take(count)
        .copied()
        .collect();
    let next = selected.last().copied().or(cursor);
    Ok((selected, next))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_the_hourly_trigger_and_clock() {
        assert_eq!(scheduled_hour(CRON, 7_200_000.0), Some(2));
        assert_eq!(scheduled_hour("5 * * * *", 7_200_000.0), None);
        assert_eq!(scheduled_hour(CRON, f64::NAN), None);
        assert_eq!(scheduled_hour(CRON, -1.0), None);
    }

    #[test]
    fn empty_and_zero_limit_preserve_cursor() {
        assert_eq!(select(&[], Some("b"), 3), Ok((vec![], Some("b"))));
        assert_eq!(select(&["b", "a"], None, 0), Ok((vec![], None)));
        assert_eq!(select(&["b", "a"], Some("x"), 0), Ok((vec![], Some("x"))));
    }

    #[test]
    fn ordering_is_deterministic_and_duplicate_keys_fail() {
        assert_eq!(
            select(&["c", "a", "b"], None, 2),
            Ok((vec!["a", "b"], Some("b")))
        );
        assert_eq!(
            select(&["b", "c", "a"], None, 2),
            Ok((vec!["a", "b"], Some("b")))
        );
        assert_eq!(select(&["a", "b", "a"], None, 2), Err(DuplicateKey));
        assert_eq!(select(&["a", "a"], None, 0), Err(DuplicateKey));
    }

    #[test]
    fn wraps_without_duplicates_and_handles_absent_cursor() {
        assert_eq!(
            select(&["c", "a", "b"], Some("b"), 9),
            Ok((vec!["c", "a", "b"], Some("b")))
        );
        assert_eq!(
            select(&["c", "a", "b"], Some("bb"), 2),
            Ok((vec!["c", "a"], Some("a")))
        );
        assert_eq!(
            select(&["c", "a"], Some("z"), 1),
            Ok((vec!["a"], Some("a")))
        );
    }

    #[test]
    fn cursor_advances_past_unchanged_full_batch() {
        let keys = ["e", "a", "d", "b", "c"];
        let (first, cursor) = select(&keys, None, 2).unwrap();
        let (second, cursor) = select(&keys, cursor, 2).unwrap();
        let (third, _) = select(&keys, cursor, 2).unwrap();
        assert_eq!(first, ["a", "b"]);
        assert_eq!(second, ["c", "d"]);
        assert_eq!(third, ["e", "a"]);
    }
}
