//! Credential-independent cleanup receipts, not a deployed retention schedule.

use nntune_api::videolinks::retention::{
    cleanup_metadata, CleanupError, MetadataBacklog, RetentionStore, CLEANUP_SAFETY_WINDOW_S,
    MAX_CLEANUP_BATCHES,
};
use nntune_api::videolinks::validation::MAX_PURGES_PER_CALL;
use std::collections::VecDeque;
use std::future::Future;
use std::task::{Context, Poll, Waker};

const NOW: i64 = 1_700_000_000;

fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut cx) {
        Poll::Ready(result) => result,
        Poll::Pending => panic!("fixture futures must complete synchronously"),
    }
}

struct Store {
    purges: VecDeque<Result<u64, CleanupError>>,
    backlog: Result<MetadataBacklog, CleanupError>,
    events: Vec<&'static str>,
}

impl RetentionStore for Store {
    async fn purge_metadata(&mut self, now: i64) -> Result<u64, CleanupError> {
        assert_eq!(now, NOW);
        self.events.push("purge");
        self.purges.pop_front().expect("bounded calls")
    }

    async fn metadata_backlog(&mut self, now: i64) -> Result<MetadataBacklog, CleanupError> {
        assert_eq!(now, NOW);
        self.events.push("audit");
        self.backlog.clone()
    }
}

fn store(purges: Vec<Result<u64, CleanupError>>) -> Store {
    Store {
        purges: purges.into(),
        backlog: Ok(MetadataBacklog {
            retained_rows: 3,
            due_rows: 2,
            oldest_due_validated_at: Some(NOW - 3_000_000),
            expired_rows: 1,
            oldest_expired_validated_at: Some(NOW - 3_000_000),
        }),
        events: Vec::new(),
    }
}

#[test]
fn zero_purge_still_audits_locked_backlog_without_claiming_it_is_drained() {
    let mut store = store(vec![Ok(0)]);
    let report = block_on(cleanup_metadata(&mut store, NOW)).unwrap();
    assert_eq!((report.batches, report.purged_rows), (1, 0));
    assert_eq!(report.evaluated_at, NOW);
    assert_eq!(report.safety_window_s, CLEANUP_SAFETY_WINDOW_S);
    assert_eq!(report.backlog, Some(store.backlog.clone().unwrap()));
    assert_eq!(store.events, ["purge", "audit"]);
}

#[test]
fn caps_cleanup_batches_and_audits_after_the_last_acknowledged_write() {
    let mut store = store(vec![Ok(MAX_PURGES_PER_CALL as u64); MAX_CLEANUP_BATCHES]);
    let report = block_on(cleanup_metadata(&mut store, NOW)).unwrap();
    assert_eq!(report.batches, MAX_CLEANUP_BATCHES);
    assert_eq!(
        report.purged_rows,
        MAX_CLEANUP_BATCHES as u64 * MAX_PURGES_PER_CALL as u64
    );
    assert!(store.purges.is_empty());
    assert_eq!(store.events.last(), Some(&"audit"));
    assert!(report.backlog.unwrap().expired_rows > 0);
}

#[test]
fn later_write_failure_retains_only_acknowledged_progress_without_retry() {
    let mut store = store(vec![Ok(200), Err(CleanupError::Store)]);
    let failure = block_on(cleanup_metadata(&mut store, NOW)).unwrap_err();
    assert_eq!(failure.error, CleanupError::Store);
    assert_eq!(
        (failure.progress.batches, failure.progress.purged_rows),
        (1, 200)
    );
    assert!(failure.progress.backlog.is_none());
    assert_eq!(store.events, ["purge", "purge"]);
}

#[test]
fn failed_audit_is_not_a_successful_retention_receipt() {
    let mut store = store(vec![Ok(7), Ok(0)]);
    store.backlog = Err(CleanupError::Store);
    let failure = block_on(cleanup_metadata(&mut store, NOW)).unwrap_err();
    assert_eq!(failure.error, CleanupError::Store);
    assert_eq!(
        (failure.progress.batches, failure.progress.purged_rows),
        (2, 7)
    );
    assert!(failure.progress.backlog.is_none());
    assert_eq!(store.events, ["purge", "purge", "audit"]);
}

#[test]
fn invalid_clocks_and_unbounded_store_receipts_fail_closed() {
    for now in [
        0,
        -1,
        i64::MIN,
        i64::MAX - CLEANUP_SAFETY_WINDOW_S + 1,
        i64::MAX,
    ] {
        let mut store = store(Vec::new());
        let failure = block_on(cleanup_metadata(&mut store, now)).unwrap_err();
        assert_eq!(failure.error, CleanupError::InvalidTime);
        assert_eq!(
            (failure.progress.batches, failure.progress.purged_rows),
            (0, 0)
        );
        assert!(store.events.is_empty());
    }
    let mut store = store(vec![Ok(MAX_PURGES_PER_CALL as u64 + 1)]);
    let failure = block_on(cleanup_metadata(&mut store, NOW)).unwrap_err();
    assert_eq!(failure.error, CleanupError::InvalidReceipt);
    assert_eq!(
        (failure.progress.batches, failure.progress.purged_rows),
        (0, 0)
    );
    assert!(failure.progress.backlog.is_none());
    assert_eq!(store.events, ["purge"]);
}
