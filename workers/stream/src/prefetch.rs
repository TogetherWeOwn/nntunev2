//! Prefetch: when a play misses and pool capacity is idle, queue the next
//! track on the [`Lane::Prefetch`](crate::pool::Lane) lane so the following
//! play is a cache hit. Prefetch never steals the last free connection from
//! a potential playback: it only runs while strictly idle capacity exists.

/// True when a prefetch may take a pool connection without starving playback.
pub fn should_prefetch(pool_in_use: u32, pool_capacity: u32) -> bool {
    pool_in_use < pool_capacity
}

/// The track after `current` in the play `queue`, if any.
pub fn next_track(queue: &[String], current: usize) -> Option<&str> {
    queue.get(current.checked_add(1)?).map(|s| s.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefetch_only_on_strictly_idle_capacity() {
        assert!(should_prefetch(0, 4));
        assert!(should_prefetch(3, 4));
        assert!(!should_prefetch(4, 4));
    }

    #[test]
    fn next_track_walks_the_queue() {
        let queue = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(next_track(&queue, 0), Some("b"));
        assert_eq!(next_track(&queue, 1), Some("c"));
        assert_eq!(next_track(&queue, 2), None);
        assert_eq!(next_track(&queue, 9), None);
    }
}
