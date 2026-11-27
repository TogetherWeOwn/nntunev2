//! Prefetch `next_track` overflow edge: `usize::MAX` fails closed.
//!
//! Pins `next_track` (`workers/stream/src/prefetch.rs:17`,
//! `current.checked_add(1)?`): adding one to `usize::MAX` must return `None`
//! instead of panicking (debug overflow) or wrapping to index 0. Boundary
//! controls prove the guard is not over-broad: `len - 1` is `None` (no next),
//! `len - 2` is the last track.
//!
//! Failing command:
//! `cargo test --locked -p nntune-stream --test prefetch_next_track`
//!
//! Pure index arithmetic: no sockets, providers, or Durable Objects.

use nntune_stream::prefetch::next_track;
use nntune_stream::{
    pool::{Lane, Pool, HARD_MAX_CONNS},
    prefetch::should_prefetch,
};

fn queue() -> Vec<String> {
    vec!["a".to_string(), "b".to_string(), "c".to_string()]
}

/// `usize::MAX + 1` overflows: the checked add must fail closed to `None`.
#[test]
fn usize_max_current_returns_none_without_panic() {
    assert_eq!(next_track(&queue(), usize::MAX), None);
}

/// At the last track there is no next track.
#[test]
fn last_index_returns_none() {
    let q = queue();
    assert_eq!(next_track(&q, q.len() - 1), None);
}

/// One before the last track yields the last track.
#[test]
fn second_last_index_returns_last_track() {
    let q = queue();
    assert_eq!(next_track(&q, q.len() - 2), Some("c"));
}

#[test]
fn empty_and_out_of_bounds_queues_have_no_successor() {
    assert_eq!(next_track(&[], 0), None);
    let q = queue();
    assert_eq!(next_track(&q, q.len()), None);
}

#[test]
fn prefetch_reserves_playback_at_the_hard_pool_ceiling() {
    let mut pool = Pool::<()>::new(u32::MAX);
    let leases: Vec<_> = (0..HARD_MAX_CONNS - 2)
        .map(|_| pool.try_acquire(Lane::Playback).unwrap())
        .collect();
    assert!(should_prefetch(&pool));
    let _prefetch = pool.try_acquire(Lane::Prefetch).unwrap();
    assert!(!should_prefetch(&pool));
    let _playback = pool.try_acquire(Lane::Playback).unwrap();
    assert!(!pool.can_acquire(Lane::Playback));
    drop(leases);
}
