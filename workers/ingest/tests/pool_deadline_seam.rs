//! Pool deadline seam: executed race pins plus exact literal/wiring pins.
//!
//! Supplement to the paired `pool_stub_deadline` suite (owned by its card
//! and never edited here): that suite proves a `Delay` bounds each stub
//! call, while this file proves the bound is correctly sized, correctly
//! wired at BOTH stages, and actually executed.
//!
//! - `race_against_deadline` (in `workers/ingest/src/pool_deadline.rs`) is
//!   driven with ready/pending fakes: fast op wins, fired deadline aborts,
//!   deadline wins ties, and one deadline spans two sequential races.
//! - Exact literals: probe `30_000` (500 `STAT`s at 50 ms + 5 s dial, at the
//!   delivery budget) and body `25_000` (5 s dial + 10 s article + 5 s
//!   redial dial + 5 s headroom), each with its derivation assert.
//! - Wiring: both `call_pool_probe_with_deadline` and
//!   `call_pool_body_with_deadline` race headers AND the body drain through
//!   the helper and map BOTH expiries to the existing `Retry` with
//!   `DEFAULT_RETRY_SECS` (no new variant).
//!
//! Sensitivity: remapping expiry `Retry -> Rejected`, dropping the
//! body-stage race, raising either constant, or awaiting the fetch
//! directly (Delay never polled) fails at least one test below.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked -p nntune-ingest --test pool_deadline_seam
//! ```

use nntune_ingest::pool_deadline::race_against_deadline;

// ---------------------------------------------------------------------------
// Production source helpers (pre-test source only).
// ---------------------------------------------------------------------------

fn production_part(source: &str) -> &str {
    source
        .split_once("#[cfg(test)]")
        .map(|(head, _)| head)
        .unwrap_or(source)
}

fn probe_source() -> &'static str {
    include_str!("../src/pool_probe.rs")
}

fn body_source() -> &'static str {
    include_str!("../src/pool_body.rs")
}

fn seam_source() -> &'static str {
    include_str!("../src/pool_deadline.rs")
}

// ---------------------------------------------------------------------------
// Executed seam: the generic race, driven with fakes (no clock, no stub).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fast_operation_wins_before_the_deadline() {
    let mut deadline = Box::pin(std::future::pending::<()>());
    let out = race_against_deadline(std::future::ready(42u32), &mut deadline).await;
    assert_eq!(out, Some(42u32));
}

#[tokio::test]
async fn fired_deadline_aborts_a_hanging_operation() {
    let mut deadline = Box::pin(std::future::ready(()));
    let out = race_against_deadline(std::future::pending::<u32>(), &mut deadline).await;
    assert_eq!(out, None);
}

#[tokio::test]
async fn deadline_wins_when_both_are_ready() {
    // Deadline-first polling: an already-fired deadline aborts even when
    // the operation is also ready, so no completed prefix escapes.
    let mut deadline = Box::pin(std::future::ready(()));
    let out = race_against_deadline(std::future::ready(7u32), &mut deadline).await;
    assert_eq!(out, None);
}

#[tokio::test]
async fn one_deadline_spans_two_sequential_races() {
    // Headers win against a live deadline, then a fired deadline aborts
    // the stalled body drain: partial progress never re-arms the budget.
    // (The same-deadline reuse shape itself is pinned by the wiring tests
    // below; awaiting pending-vs-pending here would hang, so each stage
    // is driven against the deadline state it exercises.)
    let mut live = Box::pin(std::future::pending::<()>());
    let headers = race_against_deadline(std::future::ready(1u32), &mut live).await;
    assert_eq!(headers, Some(1u32));
    let mut fired = Box::pin(std::future::ready(()));
    let stalled = race_against_deadline(std::future::pending::<u32>(), &mut fired).await;
    assert_eq!(stalled, None);
}

/// A deadline that is fired on every poll (unlike `std::future::Ready`,
/// which panics when polled after completion): mirrors `worker::Delay`
/// after it fires, which polls `Ready` again without panicking.
struct AlwaysFired;

impl std::future::Future for AlwaysFired {
    type Output = ();
    fn poll(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        std::task::Poll::Ready(())
    }
}

#[tokio::test]
async fn fired_deadline_stays_fired_for_the_second_stage() {
    // An expired deadline aborts BOTH stages when reused: the body drain
    // cannot slip through after headers expired.
    let mut deadline = AlwaysFired;
    assert_eq!(
        race_against_deadline(std::future::ready(1u32), &mut deadline).await,
        None
    );
    assert_eq!(
        race_against_deadline(std::future::ready(2u32), &mut deadline).await,
        None
    );
}

// ---------------------------------------------------------------------------
// Exact literal pins.
// ---------------------------------------------------------------------------

#[test]
fn probe_deadline_literal_is_30s() {
    assert!(
        production_part(probe_source()).contains("pub const POOL_PROBE_DEADLINE_MS: u64 = 30_000;"),
        "probe deadline must stay exactly 30_000 ms (500 STATs at 50 ms + 5 s dial)"
    );
}

#[test]
fn body_deadline_literal_is_25s() {
    assert!(
        production_part(body_source()).contains("pub const POOL_BODY_DEADLINE_MS: u64 = 25_000;"),
        "body deadline must stay exactly 25_000 ms (dial + article + redial + headroom)"
    );
}

#[test]
fn probe_deadline_derivation_is_pinned() {
    let prod = production_part(probe_source());
    assert!(
        prod.contains("PROBE_PER_STAT_ALLOWANCE_MS"),
        "probe sizing must keep its named per-STAT allowance"
    );
    assert!(
        prod.contains("PROBE_DIAL_ALLOWANCE_MS"),
        "probe sizing must keep its named dial allowance"
    );
    assert!(
        prod.contains("MAX_PROBE_SEGMENTS"),
        "probe deadline must derive from MAX_PROBE_SEGMENTS"
    );
}

#[test]
fn body_deadline_derivation_is_pinned() {
    let prod = production_part(body_source());
    assert!(
        prod.contains("BODY_DIAL_ALLOWANCE_MS"),
        "body sizing must keep its named dial allowance"
    );
    assert!(
        prod.contains("BODY_READ_BUDGET_MS"),
        "body sizing must keep its named article budget"
    );
}

// ---------------------------------------------------------------------------
// Wiring pins: both stages race through the helper, both expiries Retry.
// ---------------------------------------------------------------------------

#[test]
fn probe_races_both_stages_and_expires_to_retry() {
    let prod = production_part(probe_source());
    assert!(
        prod.contains("call_pool_probe_with_deadline(env, req, POOL_PROBE_DEADLINE_MS)"),
        "production probe entry must wire the 30 s default"
    );
    assert!(
        prod.contains("race_against_deadline(fetch, &mut deadline)"),
        "probe headers fetch must race through the shared deadline helper"
    );
    assert!(
        prod.contains("race_against_deadline(body_read, &mut deadline)"),
        "probe body drain must reuse the SAME deadline (body-stage race removal fails here)"
    );
    assert!(
        prod.contains("let expired = PoolProbeError::Retry {"),
        "probe expiry must reuse the existing Retry variant"
    );
    assert!(
        prod.contains("after_secs: DEFAULT_RETRY_SECS,"),
        "probe expiry must use DEFAULT_RETRY_SECS"
    );
    assert!(
        prod.matches("Some(Err(_)) | None => return Err(expired)")
            .count()
            >= 2,
        "BOTH probe stages (headers + body) must map expiry/transport failure to expired Retry"
    );
}

#[test]
fn body_races_both_stages_and_expires_to_retry() {
    let prod = production_part(body_source());
    assert!(
        prod.contains("call_pool_body_with_deadline(env, req, POOL_BODY_DEADLINE_MS)"),
        "production body entry must wire the 25 s default"
    );
    assert!(
        prod.contains("race_against_deadline(fetch, &mut deadline)"),
        "body headers fetch must race through the shared deadline helper"
    );
    assert!(
        prod.contains("race_against_deadline(body_read, &mut deadline)"),
        "body drain must reuse the SAME deadline (body-stage race removal fails here)"
    );
    assert!(
        prod.contains("let expired = PoolBodyError::Retry {"),
        "body expiry must reuse the existing Retry variant"
    );
    assert!(
        prod.contains("after_secs: DEFAULT_RETRY_SECS,"),
        "body expiry must use DEFAULT_RETRY_SECS"
    );
    assert!(
        prod.matches("Some(Err(_)) | None => return Err(expired)")
            .count()
            >= 2,
        "BOTH body stages (headers + body) must map expiry/transport failure to expired Retry"
    );
}

#[test]
fn seam_polls_the_deadline_first() {
    let prod = production_part(seam_source());
    assert!(
        prod.contains("Pin::new(&mut *deadline).poll(cx).is_ready()"),
        "the shared race must poll the deadline first so a fired deadline aborts a ready operation"
    );
    assert!(
        prod.contains("return Poll::Ready(None)"),
        "deadline expiry must resolve to None (mapped to Retry by the caller)"
    );
}
