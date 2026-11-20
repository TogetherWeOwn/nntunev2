//! Inert selection of caller-supplied eligible subscription keys.
//!
//! This module does not query, schedule, persist, or dispatch work. The
//! caller must supply the complete eligible set and stable, unique keys (the
//! billing schema makes `stripe_subscription_id` unique). Keys and cursors
//! use Rust's bytewise string ordering, not a database collation.

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
