//! PROTECTED acceptance tests; the Builder must not edit this file.
//!
//! Required production seam in `nntune_ingest::library`:
//! `with_db_deadline(operation, deadline, on_expiry) -> Result<T, E>`.
//! `operation: Future<Output = Result<T, E>>`, `deadline: Future<Output = ()>`,
//! `on_expiry: FnOnce() -> E`. Neither future needs to be Unpin. The helper
//! must poll the deadline throughout the operation, drop the losing operation,
//! preserve completed results, and invoke on_expiry only on expiry. Production
//! passes a finite Worker Delay; these tests inject deterministic futures.
//!
//! The companion wiring suite requires the SAME helper around all DB awaits
//! at each edge site, and pins its caller-supplied detail-free expiry errors.
//! No database, Worker Env, clock, sleep, or runtime timeout is used here.

use nntune_ingest::library::with_db_deadline;
use nntune_ingest::metering::MeterWriteError;
use nntune_ingest::retry::{JobError, ERR_WRITE_FAILED};
use std::cell::Cell;
use std::future::{pending, ready, Future};
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

// A bounded manual executor makes a broken race fail rather than hang CI.
fn finish<F: Future>(future: F) -> F::Output {
    let mut future = Box::pin(future);
    let mut cx = Context::from_waker(futures_util::task::noop_waker_ref());
    for _ in 0..32 {
        if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
    }
    panic!("database operation did not expire within 32 deterministic polls");
}

struct FiresAfter(u8);

impl Future for FiresAfter {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 == 0 {
            Poll::Ready(())
        } else {
            self.0 -= 1;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

#[test]
fn stalled_connect_expires_to_unit_error() {
    assert_eq!(
        finish(with_db_deadline(
            pending::<Result<(), ()>>(),
            FiresAfter(3),
            || ()
        )),
        Err(())
    );
}

#[test]
fn stalled_admission_query_expires_to_detail_free_transport_error() {
    let error = finish(with_db_deadline(
        pending::<Result<u64, MeterWriteError>>(),
        FiresAfter(3),
        || MeterWriteError::Transport("admission ledger unavailable".to_string()),
    ))
    .unwrap_err();
    assert!(
        matches!(error, MeterWriteError::Transport(ref detail) if detail == "admission ledger unavailable")
    );
    assert_eq!(error.http_status(), 503);
    assert!(error.is_retryable());
}

#[test]
fn stalled_upsert_expires_to_detail_free_transient_job_error() {
    let error = finish(with_db_deadline(
        pending::<Result<u64, JobError>>(),
        FiresAfter(3),
        || JobError::transient(ERR_WRITE_FAILED, "library database unavailable"),
    ))
    .unwrap_err();
    assert_eq!(error.code, "write_failed");
    assert_eq!(error.detail, "library database unavailable");
    assert!(error.transient);
    assert!(!error.made_progress);
}

#[test]
fn release_expiry_can_be_ignored_and_execution_continues() {
    let reached_after_release = finish(async {
        let _ = with_db_deadline(pending::<Result<(), ()>>(), FiresAfter(3), || ()).await;
        true
    });
    assert!(reached_after_release);
}

#[test]
fn pending_operation_waits_until_deadline_actually_fires() {
    let mut future = Box::pin(with_db_deadline(
        pending::<Result<(), ()>>(),
        FiresAfter(3),
        || (),
    ));
    let mut cx = Context::from_waker(futures_util::task::noop_waker_ref());
    for _ in 0..3 {
        assert!(future.as_mut().poll(&mut cx).is_pending());
    }
    assert_eq!(future.as_mut().poll(&mut cx), Poll::Ready(Err(())));
}

#[test]
fn fast_success_is_preserved_without_calling_expiry_factory() {
    assert_eq!(
        finish(with_db_deadline(ready(Ok::<_, ()>(17)), pending(), || {
            panic!("unexpired operation must not evaluate its timeout error")
        })),
        Ok(17)
    );
}

#[test]
fn ready_driver_failure_preserves_existing_classification() {
    let error = finish(with_db_deadline(
        ready(Err::<(), _>(JobError::permanent(
            ERR_WRITE_FAILED,
            "library database schema mismatch",
        ))),
        pending(),
        || panic!("a completed driver failure is not a deadline expiry"),
    ))
    .unwrap_err();
    assert_eq!(error.code, "write_failed");
    assert_eq!(error.detail, "library database schema mismatch");
    assert!(!error.transient);
}

#[test]
fn non_unpin_operation_and_deadline_are_supported() {
    // Async blocks are !Unpin; the production connect/query futures need
    // this too. The deadline remains live until the operation completes.
    assert_eq!(
        finish(with_db_deadline(
            async { Ok::<_, ()>(29) },
            async { pending::<()>().await },
            || ()
        )),
        Ok(29)
    );
}

struct DropWitness(Rc<Cell<bool>>);
impl Drop for DropWitness {
    fn drop(&mut self) {
        self.0.set(true);
    }
}

#[test]
fn timeout_drops_the_inflight_operation_instead_of_detaching_it() {
    let dropped = Rc::new(Cell::new(false));
    let witness = DropWitness(dropped.clone());
    let operation = async move {
        let _witness = witness;
        pending::<Result<(), ()>>().await
    };
    assert_eq!(
        finish(with_db_deadline(operation, FiresAfter(3), || ())),
        Err(())
    );
    assert!(
        dropped.get(),
        "expired connect/query must not keep running detached"
    );
}

#[test]
fn one_deadline_bounds_the_whole_multistatement_operation() {
    // Mirrors ensure -> claim/read: completing one statement must not
    // disable the deadline for a later never-resolving statement.
    let first_statement_finished = Cell::new(false);
    let operation = async {
        ready(Ok::<_, ()>(1)).await?;
        first_statement_finished.set(true);
        pending::<Result<(), ()>>().await
    };
    assert_eq!(
        finish(with_db_deadline(operation, FiresAfter(3), || ())),
        Err(())
    );
    assert!(first_statement_finished.get());
}
