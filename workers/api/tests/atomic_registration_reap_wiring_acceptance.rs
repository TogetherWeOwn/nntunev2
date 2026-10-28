//! Wiring pins for the enrollment/reaper lock coordination.
//!
//! The executed PostgreSQL bank proves the runtime behavior against the
//! private production helpers. These native pins complement it where a
//! black-box database test cannot deterministically observe the interleave:
//! the candidate lock and the delete must share one eligibility filter, the
//! delete must stay restricted to the parents the first statement locked,
//! the reaper must sequence both statements in one READ COMMITTED
//! transaction (so the delete rechecks eligibility on a fresh snapshot),
//! and enrollment must take its parent lock before claiming any proof.
//!
//! Breaks caught: dropping SKIP LOCKED from the candidate lock, deleting by
//! candidate id without reapplying the eligibility filter, letting the two
//! statements' filters drift, collapsing the reaper to one statement, or
//! claiming proofs before the enrollment parent lock is held.

fn normalize(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn function_body(source: &str, signature: &str) -> String {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("production function missing: {signature}"));
    let rest = &source[start..];
    let end = rest[signature.len()..]
        .find("\nasync fn ")
        .map(|offset| signature.len() + offset)
        .unwrap_or(rest.len());
    normalize(&rest[..end])
}

#[test]
fn reap_candidate_lock_and_delete_share_one_filter_and_locked_ids() {
    let lock_sql = normalize(&nntune_api::handlers::lock_unverified_users_sql());
    let delete_sql = normalize(&nntune_api::handlers::reap_unverified_users_sql());

    let filter = lock_sql
        .strip_prefix("SELECT users.id FROM users WHERE ")
        .and_then(|rest| rest.strip_suffix(" FOR UPDATE SKIP LOCKED"))
        .expect("candidate lock must skip parents held by enrollment, never wait on them");
    assert_eq!(
        delete_sql,
        format!("DELETE FROM users WHERE {filter} AND users.id = ANY($4)"),
        "the delete must reapply the exact candidate eligibility filter, \
         restricted to the parents the first statement locked"
    );
    assert!(
        filter.contains("NOT EXISTS (SELECT 1 FROM webauthn_credentials"),
        "a credential committed after candidate selection must exempt its user at delete time"
    );
}

#[test]
fn reaper_sequences_lock_then_fresh_recheck_delete_in_read_committed() {
    let source = include_str!("../src/worker_entry.rs");
    let body = function_body(source, "async fn reap_unverified_users_on(");
    assert!(
        body.contains("IsolationLevel::ReadCommitted"),
        "each reaper statement needs its own snapshot so the delete sees \
         credentials committed after candidate selection began"
    );
    let lock_at = body
        .find("handlers::lock_unverified_users_sql()")
        .expect("reaper must lock eligible parents with the production candidate statement");
    let delete_at = body
        .find("handlers::reap_unverified_users_sql()")
        .expect("reaper must delete with the production recheck statement");
    assert!(
        lock_at < delete_at,
        "parents must be locked before the fresh-snapshot delete rechecks them"
    );
}

#[test]
fn enrollment_locks_parent_before_claiming_any_proof() {
    let source = include_str!("../src/worker_entry.rs");
    let body = function_body(source, "async fn complete_registration_on(");
    let parent_lock_at = body
        .find("SELECT id FROM users WHERE id = $1 FOR KEY SHARE")
        .expect("enrollment must hold a parent lock the reaper's FOR UPDATE cannot pass");
    let first_claim_at = body
        .find("claim_single_use_on(&transaction, claim)")
        .expect("enrollment must claim its single-use proofs inside the transaction");
    assert!(
        parent_lock_at < first_claim_at,
        "the parent lock must be held before any proof claim can outlive its expiry"
    );
}
