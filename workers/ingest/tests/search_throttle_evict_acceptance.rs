//! Search-throttle map-cap pin (tests-only).
//!
//! - `SEARCH_THROTTLE_MAX_KEYS == 4096` per dimension (`by_ip`, `by_account`)
//!   in [`SearchThrottleStore`](nntune_ingest::rate_limit::SearchThrottleStore);
//! - filling the store to exactly 4096 tracked keys keeps every key tracked
//!   (no early clear: a spent 1-token bucket still denies on re-check);
//!
//! Run only this file:
//!
//! ```sh
//! cd workers/ingest && cargo test --locked --test search_throttle_evict_acceptance
//! ```
//!
//! Keys are synthetic opaque strings (`st-ip-NNNNN` / `st-user-NNNNN`): no
//! real IPs, no credential material anywhere in this file.

use nntune_ingest::rate_limit::{
    SearchDecision, SearchRateLimitConfig, SearchThrottleKey, SearchThrottleStore,
    SEARCH_THROTTLE_MAX_KEYS,
};

/// Cap under test: `SEARCH_THROTTLE_MAX_KEYS == 4096`.
const CAP: usize = 4096;

/// Fixed clock for the whole pin: 1-token buckets never refill at the same
/// `now_s`, so Allow/Deny is a pure function of tracked-vs-cleared state.
const NOW: u64 = 1_700_000_000;

/// One-token budget: the first check for a fresh key Allows, the immediate
/// re-check at the same timestamp Denies. Any Allow on re-check therefore
/// indicates the key lost its accounting.
fn one_shot_cfg() -> SearchRateLimitConfig {
    SearchRateLimitConfig {
        max_attempts: 1,
        window_secs: 3600,
    }
}

fn key_for(i: usize) -> SearchThrottleKey {
    SearchThrottleKey::new(&format!("st-ip-{i:05}"), &format!("st-user-{i:05}"))
}

fn is_allow(d: &SearchDecision) -> bool {
    matches!(d, SearchDecision::Allow)
}

#[test]
fn search_throttle_max_keys_is_4096() {
    assert_eq!(
        SEARCH_THROTTLE_MAX_KEYS, CAP,
        "search-throttle map cap must stay 4096 per dimension"
    );
    assert_eq!(CAP, 4096, "pin body must target exactly 4096 keys");
}

#[test]
fn tracks_all_keys_at_exactly_cap() {
    let cfg = one_shot_cfg();
    let mut store = SearchThrottleStore::new();
    // Insert the first CAP-1 keys: each is fresh, so each Allows.
    for i in 0..CAP - 1 {
        assert!(
            is_allow(&store.check(&key_for(i), &cfg, NOW)),
            "first check for distinct key {i} must Allow"
        );
    }
    for probe in [0, 1, CAP - 2] {
        assert!(
            !is_allow(&store.check(&key_for(probe), &cfg, NOW)),
            "key {probe} must still be tracked at {} keys",
            CAP - 1
        );
    }
    assert!(
        is_allow(&store.check(&key_for(CAP - 1), &cfg, NOW)),
        "the 4096th distinct key must Allow"
    );
    assert!(!is_allow(&store.check(&key_for(0), &cfg, NOW)));
}

#[test]
fn overflow_preserves_spent_buckets_until_refill() {
    let cfg = one_shot_cfg();
    let mut store = SearchThrottleStore::new();
    for i in 0..CAP {
        assert!(is_allow(&store.check(&key_for(i), &cfg, NOW)));
    }
    let overflow = key_for(CAP);
    assert!(!is_allow(&store.check(&overflow, &cfg, NOW)));
    for probe in [0, 1, CAP - 1] {
        assert!(
            !is_allow(&store.check(&key_for(probe), &cfg, NOW)),
            "key {probe} must retain its spent balance"
        );
    }
    let refilled = NOW + cfg.window_secs;
    assert!(is_allow(&store.check(&overflow, &cfg, refilled)));
    assert!(!is_allow(&store.check(&overflow, &cfg, refilled)));
    let new_key = key_for(CAP + 1);
    assert!(is_allow(&store.check(&new_key, &cfg, NOW)));
    assert!(!is_allow(&store.check(&new_key, &cfg, refilled)));
}

#[test]
fn pruning_reclaims_only_fully_refilled_buckets() {
    let cfg = one_shot_cfg();
    let mut store = SearchThrottleStore::new();
    for i in 0..CAP {
        let now = if i < 2 { NOW - 1 } else { NOW };
        assert!(is_allow(&store.check(&key_for(i), &cfg, now)));
    }
    let first_refill = NOW + cfg.window_secs - 1;
    assert!(is_allow(&store.check(&key_for(CAP), &cfg, first_refill)));
    // Reclaiming two slots leaves room to pair either spent key with a fresh one.
    let spent = key_for(2);
    let fresh = key_for(CAP + 1);
    for probe in [
        SearchThrottleKey::new(spent.ip(), fresh.account()),
        SearchThrottleKey::new(fresh.ip(), spent.account()),
    ] {
        assert!(!is_allow(&store.check(&probe, &cfg, first_refill)));
    }
}

#[test]
fn caps_ip_and_account_keys_independently() {
    let mut cfg = one_shot_cfg();
    cfg.max_attempts = CAP as u32 + 1;
    for distinct_ips in [true, false] {
        let mut store = SearchThrottleStore::new();
        let key = |i: usize| {
            let (ip, account) = if distinct_ips { (i, 0) } else { (0, i) };
            SearchThrottleKey::new(&ip.to_string(), &account.to_string())
        };
        for i in 0..CAP {
            assert!(is_allow(&store.check(&key(i), &cfg, NOW)));
        }
        assert!(!is_allow(&store.check(&key(CAP), &cfg, NOW)));
        assert!(is_allow(&store.check(&key(0), &cfg, NOW)));
    }
}
