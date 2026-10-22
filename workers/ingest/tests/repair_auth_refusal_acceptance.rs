//! Repair classifier auth-refusal acceptance pins (tests-only).
//!
//! Contract under test (`classify_repair_reply` in
//! `workers/ingest/src/repair.rs`):
//!
//! - a `401` or `403` reply from the repair service binding is that service
//!   refusing this caller, not a transient state: it classifies
//!   `RepairCall::Unrepairable`, never `RepairCall::Retry`, so the job fails
//!   terminally instead of burning its attempt budget and landing in the
//!   DLQ repeating the same refusal;
//! - the status decides, whatever accompanies it: bare statuses, plain-text
//!   bodies, JSON error bodies, JSON ApiFault bodies
//!   (`{"code": ..., "message": ..., "retryable": ...}`), and bodies carrying
//!   `Retry-After`-like hints all classify the same;
//! - neighbouring mappings are unchanged: the already-terminal
//!   `400`/`404`/`409`/`413` stay `Unrepairable`, transient `429`/`500`/
//!   `502`/`503` stay `Retry`, and a well-formed `200`
//!   `{"status":"repaired"}` stays `Repaired`.
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - leaving `401`/`403` in the fallthrough `Retry` arm (today's behaviour)
//!   fails every `*_auth_refusal_is_unrepairable*` test with
//!   `Retry` where `Unrepairable` is required;
//! - honouring a body's `retryable: true` or a `Retry-After`-like hint over
//!   the status fails the ApiFault and retry-hint tests even if bare
//!   statuses are special-cased;
//! - widening the terminal arm to every non-`200` (or every `4xx`) reply
//!   fails the transient control, which keeps `429`/`5xx` retryable;
//! - replacing the existing terminal statuses instead of extending them
//!   fails the `400`/`404`/`409`/`413` control.
//!
//! Run only this file:
//!
//! ```sh
//! cd workers/ingest && cargo test --locked --test repair_auth_refusal_acceptance
//! ```

use nntune_ingest::repair::{classify_repair_reply, RepairCall};

fn assert_unrepairable(status: u16, body: &[u8]) {
    assert_eq!(
        classify_repair_reply(status, body),
        RepairCall::Unrepairable,
        "status {status} with body {body:?} must classify Unrepairable, never Retry",
        body = String::from_utf8_lossy(body)
    );
}

fn assert_retry(status: u16, body: &[u8]) {
    assert_eq!(
        classify_repair_reply(status, body),
        RepairCall::Retry,
        "status {status} with body {body:?} must classify Retry",
        body = String::from_utf8_lossy(body)
    );
}

#[test]
fn bare_401_and_403_auth_refusals_are_unrepairable() {
    for (status, body) in [
        (401u16, b"".as_slice()),
        (403, b"".as_slice()),
        (401, b"unauthorized".as_slice()),
        (403, b"forbidden".as_slice()),
        (401, b"   ".as_slice()),
        (403, b"Forbidden\n".as_slice()),
    ] {
        assert_unrepairable(status, body);
    }
}

#[test]
fn json_error_401_and_403_auth_refusals_are_unrepairable() {
    for (status, body) in [
        (401u16, br#"{"error":"unauthorized"}"#.as_slice()),
        (403, br#"{"error":"forbidden"}"#.as_slice()),
        (401, br#"{"error":"invalid_token"}"#.as_slice()),
        (403, br#"{"error":"insufficient_permissions"}"#.as_slice()),
    ] {
        assert_unrepairable(status, body);
    }
}

#[test]
fn api_fault_401_and_403_auth_refusals_are_unrepairable_even_when_body_claims_retryable() {
    // The ApiFault wire shape is {"code", "message", "retryable"}; an auth
    // refusal that (wrongly or generically) claims `retryable: true` in its
    // body must still lose to the status.
    for (status, body) in [
        (
            401u16,
            br#"{"code":"unauthorized","message":"unauthorized","retryable":false}"#.as_slice(),
        ),
        (
            403,
            br#"{"code":"forbidden","message":"forbidden","retryable":false}"#.as_slice(),
        ),
        (
            401,
            br#"{"code":"unauthorized","message":"unauthorized","retryable":true}"#.as_slice(),
        ),
        (
            403,
            br#"{"code":"forbidden","message":"forbidden","retryable":true}"#.as_slice(),
        ),
    ] {
        assert_unrepairable(status, body);
    }
}

#[test]
fn retry_after_like_bodies_do_not_make_401_or_403_retry() {
    // The classifier takes no headers, so a retry hint can only arrive in
    // the body; no spelling of it may resurrect a `Retry` for an auth
    // refusal.
    for (status, body) in [
        (
            401u16,
            br#"{"error":"unauthorized","retry_after":17}"#.as_slice(),
        ),
        (
            403,
            br#"{"error":"forbidden","retry_after_secs":17}"#.as_slice(),
        ),
        (
            401,
            br#"{"code":"unauthorized","message":"unauthorized","retryable":true,"retry_after":17}"#
                .as_slice(),
        ),
        (
            403,
            br#"{"code":"forbidden","message":"forbidden","retryable":true,"retry_after_secs":17}"#
                .as_slice(),
        ),
        (401, b"Retry-After: 17".as_slice()),
        (403, b"Retry-After: 17\nforbidden".as_slice()),
    ] {
        assert_unrepairable(status, body);
    }
}

#[test]
fn existing_terminal_statuses_stay_unrepairable() {
    // Control: the fix extends the terminal set, it does not replace it.
    for status in [400u16, 404, 409, 413] {
        assert_unrepairable(status, b"");
        assert_unrepairable(status, br#"{"error":"rejected"}"#);
    }
}

#[test]
fn transient_statuses_stay_retry() {
    // Control: the auth-refusal mapping must not swallow transient
    // replies — rate limiting and server errors keep retrying, with or
    // without a retry hint in the body.
    for (status, body) in [
        (429u16, br#"{"error":"rate_limited"}"#.as_slice()),
        (
            429,
            br#"{"code":"rate_limited","message":"too many requests","retryable":true,"retry_after":17}"#
                .as_slice(),
        ),
        (500, br#"{"error":"internal_error"}"#.as_slice()),
        (502, br#"{"error":"provider_error"}"#.as_slice()),
        (503, br#"{"error":"provider_unavailable"}"#.as_slice()),
        (503, b"".as_slice()),
    ] {
        assert_retry(status, body);
    }
}

#[test]
fn repaired_success_stays_repaired() {
    assert_eq!(
        classify_repair_reply(200, br#"{"status":"repaired"}"#),
        RepairCall::Repaired,
        "a well-formed repaired reply must stay Repaired"
    );
}
