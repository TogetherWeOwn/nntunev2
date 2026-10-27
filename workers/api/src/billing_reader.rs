//! Inert effective-plan storage reader (serving-path-unwired prerequisite).
//!
//! [`read_effective_plan`] resolves one user's [`EffectivePlan`] from the
//! live-slot subscription row: it runs the existing
//! [`subscription_live_select_statement`] through a [`SubscriptionSource`],
//! decodes the single row with the shared [`decode_subscription_row`], and
//! composes with the unchanged pure [`resolve_effective_plan`]. No
//! dispatch, handler, admission, or cap consumer calls this yet; that wiring
//! lands in a later slice, so this module has no observable behavior today.
//!
//! Identity and billing time are both caller-supplied: the authoritative
//! user id arrives from the future trusted caller (bound as the single `$1`
//! parameter, never interpolated into SQL), and billing time is the
//! `now_unix` argument. This module reads no clock and no process state for
//! policy; the only timer is the I/O budget below. One total budget
//! encloses connect, query, cardinality handling, and decoding. Zero rows
//! is absence (closed free caps); every failure class — connection, query,
//! timeout, decode, or an unexpected extra row — returns the opaque
//! [`StoreError`], never absence.

use std::future::Future;
use std::task::Poll;
use std::time::Duration;

use crate::billing::{
    resolve_effective_plan, subscription_live_select_statement, EffectivePlan, SubscriptionView,
};
use crate::handlers::StoreError;
use crate::worker_entry::with_deadline;

/// Typed access to one raw subscription row, below the shared decoder.
/// Accessors model fallible driver retrieval only: a missing column, a
/// wrong type, or a NULL mandatory field is [`StoreError`], never a
/// silent default or SQL NULL.
pub trait SubscriptionRow {
    /// Mandatory text column (`plan_id`, `status`).
    fn text(&self, column: &str) -> Result<&str, StoreError>;
    /// Mandatory boolean column (`cancel_at_period_end`).
    fn boolean(&self, column: &str) -> Result<bool, StoreError>;
    /// Nullable `BIGINT` epoch column: SQL NULL decodes to `None`, any
    /// populated value (including zero and negative epochs) to `Some`.
    fn nullable_i64(&self, column: &str) -> Result<Option<i64>, StoreError>;
}

/// The production driver adapter: the same shared decoder consumes real
/// `tokio_postgres` rows through typed retrieval, so a driver/NULL/type
/// mismatch fails closed instead of becoming a default.
impl SubscriptionRow for tokio_postgres::Row {
    fn text(&self, column: &str) -> Result<&str, StoreError> {
        self.try_get(column).map_err(|_| StoreError)
    }

    fn boolean(&self, column: &str) -> Result<bool, StoreError> {
        self.try_get(column).map_err(|_| StoreError)
    }

    fn nullable_i64(&self, column: &str) -> Result<Option<i64>, StoreError> {
        self.try_get(column).map_err(|_| StoreError)
    }
}

/// Narrow async read boundary for the live-slot subscription row. The
/// operation includes connection setup and returns raw rows (not decoded
/// views) so the reader observes cardinality itself.
pub trait SubscriptionSource {
    /// Raw row type fed to the shared decoder.
    type Row: SubscriptionRow;

    /// Run the reader-supplied SELECT with the reader-supplied bindings.
    /// One call performs at most one connection plus one query: no retry
    /// loop, no second statement, no writes. Desugared (like
    /// [`Store`](crate::handlers::Store)) so the public trait names no
    /// auto-trait bounds the native test doubles could not meet.
    fn select_subscription(
        &self,
        sql: &str,
        params: &[&str],
    ) -> impl Future<Output = Result<Vec<Self::Row>, StoreError>>;
}

/// Owned subscription projection: the six live-slot fields with strings
/// detached from the driver row's lifetime.
pub struct OwnedSubscription {
    /// Stored plan id, verbatim.
    pub plan_id: String,
    /// Stored status, verbatim.
    pub status: String,
    /// Stored scheduled-cancellation flag.
    pub cancel_at_period_end: bool,
    /// Trial end as Unix seconds, `None` where the column is NULL.
    pub trial_end_unix: Option<i64>,
    /// Last confirmed paid coverage end, `None` where the column is NULL.
    pub paid_through_at_unix: Option<i64>,
    /// Stored renewal-grace deadline, `None` where the column is NULL.
    pub grace_until_unix: Option<i64>,
}

/// Decode one raw row into the owned projection. Every field is mandatory
/// except the three nullable epochs; SQL NULL, zero, and negative epochs
/// are preserved exactly, while a missing column, a NULL mandatory field,
/// a wrong type, or a malformed epoch is an opaque failure.
pub fn decode_subscription_row<R: SubscriptionRow + ?Sized>(
    row: &R,
) -> Result<OwnedSubscription, StoreError> {
    Ok(OwnedSubscription {
        plan_id: row.text("plan_id")?.to_owned(),
        status: row.text("status")?.to_owned(),
        cancel_at_period_end: row.boolean("cancel_at_period_end")?,
        trial_end_unix: row.nullable_i64("trial_end_unix")?,
        paid_through_at_unix: row.nullable_i64("paid_through_at_unix")?,
        grace_until_unix: row.nullable_i64("grace_until_unix")?,
    })
}

/// Platform budget timer matching [`with_deadline`]'s own selection:
/// Worker `Delay` on wasm32, `futures-timer` natively. There is no
/// `Instant` here on purpose — the wasm32 target has no monotonic clock —
/// so the budget stays enforceable on both platforms.
#[cfg(target_arch = "wasm32")]
fn budget_timer(timeout_ms: u64) -> worker::Delay {
    worker::Delay::from(Duration::from_millis(timeout_ms))
}

/// Native counterpart of the wasm32 [`budget_timer`] above.
#[cfg(not(target_arch = "wasm32"))]
fn budget_timer(timeout_ms: u64) -> futures_timer::Delay {
    futures_timer::Delay::new(Duration::from_millis(timeout_ms))
}

/// Read and resolve one user's effective plan inside a single total budget.
///
/// The caller supplies the authoritative user id (bound verbatim as `$1`),
/// the billing clock (`now_unix`), and the I/O budget (`timeout_ms`).
/// Zero rows resolves to closed free caps; one row decodes and resolves
/// through the existing policy; any failure — including an unexpected
/// second row — is an opaque [`StoreError`].
///
/// The budget is observed twice but armed once: [`with_deadline`] preempts
/// stalled async I/O (its timer is polled first, so expiry wins ties),
/// while `spent` — armed here for the same duration — is re-polled after
/// the blocking decode, which no executor can preempt mid-call. A decode
/// that finishes at or past the budget is therefore a timeout, not late
/// entitlement. Both timers share one arm point and one duration; nothing
/// re-arms or extends either, and the operation holds no retry loop or
/// detached task, so timeout and caller cancellation release the pending
/// I/O and rows before the caller observes the result.
pub async fn read_effective_plan<S: SubscriptionSource>(
    source: &S,
    authoritative_user_id: &str,
    now_unix: i64,
    timeout_ms: u64,
) -> Result<EffectivePlan, StoreError> {
    let params = [authoritative_user_id];
    // Arm the post-decode timer now: Worker `Delay` starts on first poll,
    // so this poll fixes the shared arm point for both timers.
    let mut spent = std::pin::pin!(budget_timer(timeout_ms));
    std::future::poll_fn(|cx| {
        let _armed = spent.as_mut().poll(cx);
        Poll::Ready(())
    })
    .await;
    let selected = with_deadline(
        async {
            let rows = source
                .select_subscription(subscription_live_select_statement(), &params)
                .await?;
            if rows.len() > 1 {
                return Err(StoreError);
            }
            let decoded = {
                let Some(row) = rows.into_iter().next() else {
                    return Ok(None);
                };
                decode_subscription_row(&row)?
            };
            Ok(Some(decoded))
        },
        timeout_ms,
        StoreError,
    )
    .await?;
    // The race above cannot preempt the blocking decode it just awaited:
    // re-poll the co-armed timer so a decode that consumed the budget
    // reports a timeout instead of returning late entitlement.
    let expired = std::future::poll_fn(|cx| Poll::Ready(spent.as_mut().poll(cx).is_ready())).await;
    if expired {
        return Err(StoreError);
    }
    let Some(owned) = selected else {
        return Ok(resolve_effective_plan(None, now_unix));
    };
    Ok(resolve_effective_plan(
        Some(SubscriptionView {
            plan_id: &owned.plan_id,
            status: &owned.status,
            cancel_at_period_end: owned.cancel_at_period_end,
            trial_end_unix: owned.trial_end_unix,
            paid_through_at_unix: owned.paid_through_at_unix,
            grace_until_unix: owned.grace_until_unix,
        }),
        now_unix,
    ))
}
