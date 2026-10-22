//! Independently owned live-loader capacity-derivation acceptance tests.
//! Oracle: pinned seam S:343 (whole-object deny, never truncate), S:345-354
//! (bounds table), S:353/P1:226 (90% refusal, no live eviction), S:360/P2:404
//! (<=50 cap, playback-first headroom, quarantine, metering, backpressure),
//! P1:184 (cap formula, disabled-plan denial before clamp). See
//! loader-slice-acceptance.md for the oracle, seam, matrix and proof limits.
//! This handoff is intentionally RED until the portable seam exists.
//! All inputs are inert synthetic counts/sizes; no clock, DB, vault or pool.

use nntune_stream::loader::{
    derive_capacity, CapacityGrant, CapacityRequest, LoaderDenial, MAX_ACTION_ROWS,
    MAX_AUDIT_OUTBOX, MAX_BLOCK_BYTES, MAX_CAPABILITY_BYTES, MAX_DECODED_ARTICLE_BYTES,
    MAX_EFFECTIVE_CONNECTIONS, MAX_ENCODED_ARTICLE_BYTES, MAX_EXTRA_BUFFER_BYTES,
    MAX_GLOBAL_SCOPES, MAX_ISSUE_ATTEMPTS_PER_OWNER_PER_MIN, MAX_MANIFEST_CHECK_BYTES,
    MAX_MANIFEST_RESPONSE_BYTES, MAX_METADATA_BYTES, MAX_NESTED_RESERVE_BYTES,
    MAX_NONTERMINAL_JTIS_PER_OWNER, MAX_OPEN_SCOPES_PER_OWNER, MAX_PLAN_ACTIONS, MAX_PLAN_BYTES,
    MAX_PROOF_BYTES, MAX_PROOF_NONCES, MAX_PROVIDER_REQUEST_BYTES, MAX_RETAINED_JTIS,
    MAX_SCOPE_DEPTH, MAX_SCOPE_ENVELOPE_BYTES, MAX_SCOPE_TARGETS, MIN_PLAN_ACTIONS,
    REFUSAL_USE_PERMILLE,
};
use nntune_stream::pool::{effective_cap, Lane};

fn request() -> CapacityRequest {
    CapacityRequest {
        object_bytes: 100,
        bound_bytes: MAX_PROVIDER_REQUEST_BYTES,
        provider_cap: 10,
        plan_cap: 10,
        lane: Lane::Playback,
        in_flight: 0,
        quarantined: false,
        global_used: 0,
        global_bound: MAX_RETAINED_JTIS,
        owner_used: 0,
        owner_bound: MAX_NONTERMINAL_JTIS_PER_OWNER,
        resolve_used: 0,
    }
}

fn grant(req: &CapacityRequest) -> CapacityGrant {
    match derive_capacity(req) {
        Ok(grant) => grant,
        Err(denial) => panic!("admissible capacity denied: {denial}"),
    }
}

fn deny(req: &CapacityRequest) -> LoaderDenial {
    match derive_capacity(req) {
        Ok(_) => panic!("inadmissible capacity granted"),
        Err(denial) => denial,
    }
}

fn deny_unavailable(req: &CapacityRequest) {
    assert_eq!(deny(req), LoaderDenial::ProviderUnavailable);
}

#[test]
fn clean_request_grants_with_echoed_whole_size_and_metered_slot() {
    let req = request();
    let grant = grant(&req);
    assert_eq!(grant.effective_max_connections(), 10);
    // The grant carries the whole admitted size, never a truncated prefix.
    assert_eq!(grant.object_bytes(), 100);
    // Metering input: the admitted slot count after this grant.
    assert_eq!(grant.admitted_slot(), 1);
}

#[test]
fn object_over_bound_denies_whole_object_at_every_table_byte_bound() {
    // S:345-354 byte rows ride the one generic (actual, bound) mechanism:
    // fit is inclusive, bound+1 denies, and no partial grant exists.
    for bound in [
        MAX_PROVIDER_REQUEST_BYTES,
        MAX_CAPABILITY_BYTES,
        MAX_PROOF_BYTES,
        MAX_NESTED_RESERVE_BYTES,
        MAX_SCOPE_ENVELOPE_BYTES,
        MAX_PLAN_BYTES,
        MAX_METADATA_BYTES,
        MAX_MANIFEST_RESPONSE_BYTES,
        MAX_MANIFEST_CHECK_BYTES,
        MAX_ENCODED_ARTICLE_BYTES,
        MAX_DECODED_ARTICLE_BYTES,
        MAX_BLOCK_BYTES,
        MAX_EXTRA_BUFFER_BYTES,
    ] {
        let mut fit = request();
        fit.object_bytes = bound;
        fit.bound_bytes = bound;
        assert_eq!(grant(&fit).object_bytes(), bound);
        let mut over = request();
        over.object_bytes = bound.saturating_add(1);
        over.bound_bytes = bound;
        deny_unavailable(&over);
    }
    // Plaintext ceiling is u64-scale; the same inclusive edge applies.
    let mut fit = request();
    fit.object_bytes = 512 * 1024 * 1024;
    fit.bound_bytes = 512 * 1024 * 1024;
    assert_eq!(grant(&fit).object_bytes(), 512 * 1024 * 1024);
    let mut over = request();
    over.object_bytes = 512 * 1024 * 1024 + 1;
    over.bound_bytes = 512 * 1024 * 1024;
    deny_unavailable(&over);
}

#[test]
fn table_byte_bound_values_match_the_pinned_contract() {
    assert_eq!(MAX_PROVIDER_REQUEST_BYTES, 8 * 1024);
    assert_eq!(MAX_CAPABILITY_BYTES, 4 * 1024);
    assert_eq!(MAX_PROOF_BYTES, 2 * 1024);
    assert_eq!(MAX_NESTED_RESERVE_BYTES, 8 * 1024);
    assert_eq!(MAX_SCOPE_ENVELOPE_BYTES, 256 * 1024);
    assert_eq!(MAX_PLAN_BYTES, 768 * 1024);
    assert_eq!(MAX_METADATA_BYTES, 896 * 1024);
    assert_eq!(MAX_MANIFEST_RESPONSE_BYTES, 1024 * 1024);
    assert_eq!(MAX_MANIFEST_CHECK_BYTES, 1024);
    assert_eq!(MAX_ENCODED_ARTICLE_BYTES, 2 * 1024 * 1024);
    assert_eq!(MAX_DECODED_ARTICLE_BYTES, 1024 * 1024);
    assert_eq!(MAX_BLOCK_BYTES, 64 * 1024);
    assert_eq!(MAX_EXTRA_BUFFER_BYTES, 8 * 1024 * 1024);
}

#[test]
fn table_shape_counts_deny_over_whole_object() {
    // Shape ceilings (depth/targets/actions) ride the same inclusive
    // (actual, bound) mechanism as byte bounds.
    for (bound, over) in [
        (u64::from(MAX_SCOPE_DEPTH), 9),
        (u64::from(MAX_SCOPE_TARGETS), 501),
        (u64::from(MAX_PLAN_ACTIONS), 2049),
    ] {
        let mut fit = request();
        fit.object_bytes = bound;
        fit.bound_bytes = bound;
        grant(&fit);
        let mut too_many = request();
        too_many.object_bytes = over;
        too_many.bound_bytes = bound;
        deny_unavailable(&too_many);
    }
    assert_eq!(MAX_SCOPE_DEPTH, 8);
    assert_eq!(MAX_SCOPE_TARGETS, 500);
    assert_eq!(MAX_PLAN_ACTIONS, 2048);
    assert_eq!(MIN_PLAN_ACTIONS, 1);
}

#[test]
fn effective_cap_is_the_minimum_clamped_to_fifty() {
    for (provider, plan, effective) in [
        (3, 10, 3),
        (10, 3, 3),
        (1, 1, 1),
        (50, 50, 50),
        (50, 10_000, 50),
        (10_000, 50, 50),
    ] {
        let mut req = request();
        req.provider_cap = provider;
        req.plan_cap = plan;
        assert_eq!(
            grant(&req).effective_max_connections(),
            effective,
            "provider {provider} plan {plan}"
        );
    }
    assert_eq!(MAX_EFFECTIVE_CONNECTIONS, 50);
}

#[test]
fn nonpositive_or_oversize_caps_deny_before_any_clamp() {
    // P1:184: validate each cap positive and the provider cap at most 50
    // before the min/50 clamp. A zero plan is disabled entitlement, denied
    // as authorization; unusable stored values deny as unavailable.
    for (provider, plan) in [(0, 5), (5, 0), (0, 0), (-1, 5), (5, -1), (-3, -3)] {
        let mut req = request();
        req.provider_cap = provider;
        req.plan_cap = plan;
        if plan <= 0 {
            assert_eq!(deny(&req), LoaderDenial::AuthorizationDenied);
        } else {
            deny_unavailable(&req);
        }
    }
    for provider in [51, 100, i64::MAX] {
        let mut req = request();
        req.provider_cap = provider;
        deny_unavailable(&req);
    }
}

#[test]
fn disabled_plan_denial_precedes_the_existing_clamp_helper() {
    // The existing pool helper clamps zero to one; the loader must never
    // route a disabled plan through it. This pins the contrast so a future
    // refactor cannot silently delegate.
    assert_eq!(effective_cap(0, 0), 1);
    assert_eq!(effective_cap(0, 5), 1);
    let mut req = request();
    req.provider_cap = 5;
    req.plan_cap = 0;
    assert_eq!(deny(&req), LoaderDenial::AuthorizationDenied);
}

#[test]
fn global_ninety_percent_refusal_without_live_eviction() {
    assert_eq!(REFUSAL_USE_PERMILLE, 900);
    // 90% is refusal; just below admits. No eviction input exists: the
    // derivation only denies new work, never frees live records.
    let mut admit = request();
    admit.global_used = 89_999;
    admit.global_bound = MAX_RETAINED_JTIS;
    grant(&admit);
    let mut refuse = request();
    refuse.global_used = 90_000;
    refuse.global_bound = MAX_RETAINED_JTIS;
    deny_unavailable(&refuse);
    // Saturated counters must fail closed, never wrap to admit.
    let mut saturated = request();
    saturated.global_used = u64::MAX;
    saturated.global_bound = MAX_RETAINED_JTIS;
    deny_unavailable(&saturated);
    assert_eq!(MAX_RETAINED_JTIS, 100_000);
    assert_eq!(MAX_PROOF_NONCES, 500_000);
    assert_eq!(MAX_AUDIT_OUTBOX, 100_000);
    assert_eq!(MAX_GLOBAL_SCOPES, 100_000);
    assert_eq!(MAX_ACTION_ROWS, 2_000_000);
}

#[test]
fn per_owner_ceilings_deny_at_the_bound() {
    for (bound, just_below) in [
        (MAX_NONTERMINAL_JTIS_PER_OWNER, 7),
        (MAX_OPEN_SCOPES_PER_OWNER, 63),
        (MAX_ISSUE_ATTEMPTS_PER_OWNER_PER_MIN, 59),
    ] {
        let mut admit = request();
        admit.owner_used = just_below;
        admit.owner_bound = bound;
        grant(&admit);
        let mut refuse = request();
        refuse.owner_used = bound;
        refuse.owner_bound = bound;
        deny_unavailable(&refuse);
    }
    assert_eq!(MAX_NONTERMINAL_JTIS_PER_OWNER, 8);
    assert_eq!(MAX_OPEN_SCOPES_PER_OWNER, 64);
    assert_eq!(MAX_ISSUE_ATTEMPTS_PER_OWNER_PER_MIN, 60);
}

#[test]
fn second_resolve_attempt_for_a_jti_only_denies() {
    // S:353: two resolve attempts per JTI are retained, but the second can
    // only deny, so any recorded prior attempt fails a new one closed.
    let mut first = request();
    first.resolve_used = 0;
    grant(&first);
    for used in [1, 2, u32::MAX] {
        let mut retry = request();
        retry.resolve_used = used;
        deny_unavailable(&retry);
    }
}

#[test]
fn quarantine_denies_before_slot_accounting() {
    let mut req = request();
    req.quarantined = true;
    deny_unavailable(&req);
}

#[test]
fn playback_takes_the_last_slot_while_other_lanes_keep_headroom() {
    // Mirrors the pool's reserve rule exactly: Playback reserve 0,
    // Prefetch/Ingest reserve 1 (playback-first headroom).
    let mut last = request();
    last.provider_cap = 4;
    last.plan_cap = 4;
    last.in_flight = 3;
    last.lane = Lane::Playback;
    let last_grant = grant(&last);
    assert_eq!(last_grant.effective_max_connections(), 4);
    assert_eq!(last_grant.admitted_slot(), 4);
    for lane in [Lane::Prefetch, Lane::Ingest] {
        let mut held = request();
        held.provider_cap = 4;
        held.plan_cap = 4;
        held.in_flight = 3;
        held.lane = lane;
        deny_unavailable(&held);
        let mut spare = request();
        spare.provider_cap = 4;
        spare.plan_cap = 4;
        spare.in_flight = 2;
        spare.lane = lane;
        assert_eq!(grant(&spare).admitted_slot(), 3);
    }
}

#[test]
fn exhausted_capacity_denies_as_backpressure_without_queue_or_partial_grant() {
    // At cap the derivation denies; there is no wait, queue or partial
    // slot. Saturated in-flight counters fail closed, never wrap.
    let mut full = request();
    full.provider_cap = 4;
    full.plan_cap = 4;
    full.in_flight = 4;
    full.lane = Lane::Playback;
    deny_unavailable(&full);
    let mut saturated = request();
    saturated.in_flight = u32::MAX;
    deny_unavailable(&saturated);
}

#[test]
fn denial_codes_are_fixed_and_input_free() {
    // Every capacity denial renders one fixed code; sizes/caps/counts
    // never echo into the error.
    let mut over = request();
    over.object_bytes = MAX_PROVIDER_REQUEST_BYTES + 1;
    assert_eq!(deny(&over).to_string(), "provider_unavailable");
    let mut disabled = request();
    disabled.plan_cap = 0;
    assert_eq!(deny(&disabled).to_string(), "authorization_denied");
}
