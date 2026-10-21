//! Pool connection ceiling pin: `HARD_MAX_CONNS` and the clamp in
//! `effective_cap` / `Pool::new`.
//!
//! Pins the absolute provider-connection ceiling (50) against drift: caps
//! above the ceiling clamp down, zero caps clamp up to 1, and the minimum
//! wins inside the sane range. `Pool::new` applies the same clamp so a `0`
//! or `u32::MAX` capacity constructs without panic. Pure calls only: no
//! sockets, providers, or Durable Objects.

use nntune_stream::pool::{effective_cap, Pool, HARD_MAX_CONNS};

#[test]
fn hard_max_conns_is_fifty() {
    assert_eq!(HARD_MAX_CONNS, 50);
}

#[test]
fn caps_above_ceiling_clamp_to_hard_max() {
    assert_eq!(effective_cap(1000, 1000), 50);
    assert_eq!(effective_cap(u32::MAX, u32::MAX), 50);
    assert_eq!(effective_cap(u32::MAX, u32::MAX), HARD_MAX_CONNS);
}

#[test]
fn zero_caps_clamp_up_to_one() {
    assert_eq!(effective_cap(0, 0), 1);
    assert_eq!(effective_cap(0, 50), 1);
}

#[test]
fn minimum_wins_inside_sane_range() {
    assert_eq!(effective_cap(3, 7), 3);
    assert_eq!(effective_cap(7, 3), 3);
}

#[test]
fn pool_new_clamps_extreme_capacities_without_panic() {
    let zero: Pool<()> = Pool::new(0);
    assert_eq!(zero.capacity(), 1);
    let maxed: Pool<()> = Pool::new(u32::MAX);
    assert_eq!(maxed.capacity(), HARD_MAX_CONNS);
}
