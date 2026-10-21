//! Search-throttle map-cap pin (tests-only).
//!
//! Contract under test (`workers/ingest/src/rate_limit.rs:257-260`,
//! enforced at `:290-295`):
//!
//! - `SEARCH_THROTTLE_MAX_KEYS == 4096` per dimension (`by_ip`, `by_account`)
//!   in [`SearchThrottleStore`](nntune_ingest::rate_limit::SearchThrottleStore);
//! - filling the store to exactly 4096 tracked keys keeps every key tracked
//!   (no early clear: a spent 1-token bucket still denies on re-check);
//! - the 4097th distinct key triggers the documented reset — BOTH maps are
//!   cleared to fresh buckets, and the new key is then tracked.
//!
//! Wording note: the card text describes this as "the 4097th evicts the
//! oldest while staying at 4096 tracked" (single-entry LRU). The
//! implementation does not do that — it clears both maps (`by_ip.clear()` +
//! `by_account.clear()`), so after the overflow the store holds 1 key, not
//! 4096, and previously spent keys become Allow again. These tests pin the
//! ACTUAL clear behavior: the post-overflow assertions on the second-oldest
//! key (`idx 1`) distinguish full-clear (Allow) from oldest-only eviction
//! (still Deny). If single-entry LRU is wanted, that is a separate paired
//! build card — implementation stays untouched here.
//!
//! Sensitivity (implementation stays untouched; verified against a temporary
//! `SEARCH_THROTTLE_MAX_KEYS` mutation, restored before committing):
//!
//! - lowering the cap (e.g. to 8) makes `tracks_all_keys_at_exactly_cap`
//!   fail: the store clears early and a spent key Allows on re-check;
//! - lowering the cap makes `overflow_clears_both_maps_to_fresh_buckets`
//!   fail: the overflow happens before the 4097th key;
//! - raising the cap (e.g. to 1<<20) makes
//!   `overflow_clears_both_maps_to_fresh_buckets` fail: the 4097th key no
//!   longer clears, so spent keys still Deny after it;
//! - replacing the clear with single-oldest eviction makes the `idx 1`
//!   post-overflow Allow assertions fail (second-oldest would stay Deny).
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
/// proves the key's bucket is gone (cleared), not merely refilled.
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
    // At CAP-1 tracked keys no clear has run (the clear fires only when
    // `len() >= MAX` at entry): spent buckets still deny on re-check.
    for probe in [0, 1, CAP - 2] {
        assert!(
            !is_allow(&store.check(&key_for(probe), &cfg, NOW)),
            "key {probe} must still be tracked at {} keys",
            CAP - 1
        );
    }
    // The CAPth insert still sees `len() == CAP-1 < MAX`, so no clear runs
    // and every one of the 4096 keys is tracked afterwards.
    assert!(
        is_allow(&store.check(&key_for(CAP - 1), &cfg, NOW)),
        "the 4096th distinct key must Allow without triggering the reset"
    );
    // No further `check` call here on purpose: with `len() == MAX` ANY next
    // call — even a re-check of a tracked key — runs the reset first (that
    // is what the overflow test below pins).
}

#[test]
fn overflow_clears_both_maps_to_fresh_buckets() {
    let cfg = one_shot_cfg();
    let mut store = SearchThrottleStore::new();
    for i in 0..CAP {
        assert!(is_allow(&store.check(&key_for(i), &cfg, NOW)));
    }
    // The store now holds exactly CAP keys per dimension (proven by
    // `tracks_all_keys_at_exactly_cap` without a post-fill probe, since any
    // probe at `len() == MAX` would itself run the reset). The 4097th
    // distinct key therefore runs the documented clear first, then admits
    // the new key.
    let overflow = key_for(CAP);
    assert!(
        is_allow(&store.check(&overflow, &cfg, NOW)),
        "4097th distinct key must Allow (after the cap reset)"
    );
    // Full clear, not oldest-only eviction: the oldest AND the second-oldest
    // AND the last pre-overflow keys all Allow again (fresh buckets). Under
    // single-entry LRU, idx 1 and idx CAP-1 would still Deny.
    for probe in [0, 1, CAP - 1] {
        assert!(
            is_allow(&store.check(&key_for(probe), &cfg, NOW)),
            "key {probe} must be wiped by the cap reset (Allow again)"
        );
    }
    // Post-clear tracking resumes: the overflow key spent its single token,
    // so its immediate re-check denies.
    assert!(
        !is_allow(&store.check(&overflow, &cfg, NOW)),
        "overflow key must be tracked post-reset (re-check denies)"
    );
}
