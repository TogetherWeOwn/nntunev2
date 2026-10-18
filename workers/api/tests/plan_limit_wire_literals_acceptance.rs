//! Plan-limit wire literal pins (tests-only).
//!
//! Contract under test (`workers/api/src/plan_limits.rs:44-62`):
//!
//! - `UPGRADE_URL == "/pricing"` (the client's pricing route);
//! - `UPGRADE_KEY == "upgrade_url"` (body key carrying the upgrade pointer);
//! - `LIMIT_HEADER == "x-plan-limit"` (header naming the exhausted limit);
//! - `RESET_HEADER == "x-plan-reset"` (header naming the reset/retry hint);
//! - `RETRY_AFTER_HEADER == "retry-after"` (required on 429);
//! - `PLAN_LIMIT_MESSAGE == "plan limit reached"` (402 envelope message);
//! - `RATE_LIMIT_MESSAGE == "too many requests"` (429 envelope message);
//! - `CONCURRENT_RETRY_AFTER_SECS == 30`, surfaced through the
//!   concurrent-429 path (`check_concurrent` at cap).
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - any literal drift (renamed header, reworded message, moved pricing
//!   route) fails its pin below — the client-visible wire in
//!   `docs/api-errors.md` ("Limit refusal wire") must change deliberately,
//!   with the docs and the e2e `assertPlanLimitRefusal` legs updated in the
//!   same slice;
//! - lowering `CONCURRENT_RETRY_AFTER_SECS` shortens the 429 retry hint,
//!   failing `concurrent_429_carries_30s_retry_after`.
//!
//! Boundary: enforcement behavior stays covered by the existing enforcement-behavior suite; this suite pins
//! only the documented wire literals, not the caps matrix.
//!
//! Run only this file:
//!
//! ```sh
//! cd workers/api && cargo test --locked --test plan_limit_wire_literals_acceptance
//! ```

use nntune_api::plan_limits::PlanId;
use nntune_api::plan_limits::{
    check_concurrent, CONCURRENT_RETRY_AFTER_SECS, LIMIT_HEADER, PLAN_LIMIT_MESSAGE,
    RATE_LIMIT_MESSAGE, RESET_HEADER, RETRY_AFTER_HEADER, UPGRADE_KEY, UPGRADE_URL,
};

#[test]
fn upgrade_pointer_literals_are_exact() {
    assert_eq!(
        UPGRADE_URL, "/pricing",
        "upgrade CTA must stay the client's pricing route"
    );
    assert_eq!(
        UPGRADE_KEY, "upgrade_url",
        "upgrade pointer body key must match docs/api-errors.md"
    );
}

#[test]
fn refusal_header_names_are_exact() {
    assert_eq!(
        LIMIT_HEADER, "x-plan-limit",
        "limit header must stay the preferred wire name"
    );
    assert_eq!(
        RESET_HEADER, "x-plan-reset",
        "reset header must stay the preferred wire name"
    );
    assert_eq!(
        RETRY_AFTER_HEADER, "retry-after",
        "429 retry header must stay lowercase retry-after"
    );
}

#[test]
fn envelope_messages_are_exact() {
    assert_eq!(
        PLAN_LIMIT_MESSAGE, "plan limit reached",
        "402 envelope message must match docs/api-errors.md"
    );
    assert_eq!(
        RATE_LIMIT_MESSAGE, "too many requests",
        "429 envelope message must match docs/api-errors.md"
    );
}

#[test]
fn concurrent_429_carries_30s_retry_after() {
    // Through the concurrent-429 path, not just the bare constant: at-cap on
    // every tier denies 429 with retry_after_secs == 30 and a literal
    // `retry-after: 30` header alongside the `x-plan-limit` / `x-plan-reset`
    // pair.
    assert_eq!(
        CONCURRENT_RETRY_AFTER_SECS, 30,
        "concurrent 429 retry hint must stay exactly 30s"
    );
    for plan in PlanId::all() {
        let caps = plan.caps();
        let denial = check_concurrent(plan, caps.max_concurrent_streams)
            .denial()
            .expect("at-cap concurrency must deny");
        assert_eq!(denial.status, 429);
        assert_eq!(denial.retry_after_secs, Some(30));
        let headers = denial.headers();
        let get = |name: &str| {
            headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        };
        assert!(
            get(LIMIT_HEADER).is_some(),
            "{plan:?}: 429 must name the limit"
        );
        assert!(
            get(RESET_HEADER).is_some(),
            "{plan:?}: 429 must name the reset hint"
        );
        assert_eq!(
            get(RETRY_AFTER_HEADER),
            Some("30"),
            "{plan:?}: 429 retry-after header must carry exactly 30"
        );
    }
}

#[test]
fn refusal_body_carries_literal_upgrade_pointer() {
    // The serialized 402 body embeds the literal key and value (not just the
    // symbols): any drift in either literal flips this pin.
    let denial = nntune_api::plan_limits::check_ingest_write(
        PlanId::Free,
        PlanId::Free.caps().ingest_jobs_day,
    )
    .denial()
    .expect("at-cap ingest must deny");
    let body = denial.body_json();
    assert!(
        body.contains("\"upgrade_url\":\"/pricing\""),
        "402 body must embed the literal upgrade pointer, got: {body}"
    );
}
