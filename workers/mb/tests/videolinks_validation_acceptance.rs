//! Validation-runner contract. These fixtures exercise orchestration, not a
//! deployed schedule, a database connection or a live YouTube credential.

use nntune_mb::videolinks::validation::{
    refresh_due, BatchValidation, ValidationClient, ValidationError, ValidationResponse,
    ValidationStore, MAX_VALIDATIONS_PER_RUN,
};
use nntune_mb::videolinks::VALIDATION_TTL_S;
use std::cell::RefCell;
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
    ids: Vec<String>,
    events: RefCell<Vec<&'static str>>,
    batches: Vec<BatchValidation>,
    expired_metadata: bool,
    fail_cleanup: bool,
    fail_commit: bool,
}

impl Store {
    fn new(ids: Vec<String>) -> Self {
        Self {
            ids,
            events: RefCell::new(Vec::new()),
            batches: Vec::new(),
            expired_metadata: true,
            fail_cleanup: false,
            fail_commit: false,
        }
    }
}

impl ValidationStore for Store {
    async fn purge_expired(&mut self, now: i64) -> Result<u64, ValidationError> {
        assert_eq!(now, NOW);
        self.events.borrow_mut().push("purge");
        if self.fail_cleanup {
            return Err(ValidationError::Store);
        }
        self.expired_metadata = false;
        Ok(1)
    }

    async fn due_video_ids(
        &mut self,
        now: i64,
        limit: usize,
    ) -> Result<Vec<String>, ValidationError> {
        assert_eq!((now, limit), (NOW, MAX_VALIDATIONS_PER_RUN));
        assert!(!self.expired_metadata, "purge precedes queue selection");
        self.events.borrow_mut().push("due");
        Ok(self.ids.clone())
    }

    async fn apply_batch(&mut self, batch: &BatchValidation) -> Result<u64, ValidationError> {
        self.events.borrow_mut().push("apply");
        if self.fail_commit {
            return Err(ValidationError::Store);
        }
        self.batches.push(batch.clone());
        Ok(batch.requested_ids().len() as u64)
    }
}

struct Client {
    responses: RefCell<VecDeque<Result<ValidationResponse, ValidationError>>>,
    calls: RefCell<Vec<Vec<String>>>,
}

impl Client {
    fn new(responses: Vec<Result<ValidationResponse, ValidationError>>) -> Self {
        Self {
            responses: RefCell::new(responses.into()),
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl ValidationClient for Client {
    async fn videos_list(&self, ids: &[String]) -> Result<ValidationResponse, ValidationError> {
        self.calls.borrow_mut().push(ids.to_vec());
        self.responses
            .borrow_mut()
            .pop_front()
            .expect("bounded calls")
    }
}

fn response(status: u16, body: &str) -> Result<ValidationResponse, ValidationError> {
    Ok(ValidationResponse {
        status,
        body: body.to_string(),
    })
}

#[test]
fn bounds_batches_dedupes_ids_and_records_missing_videos() {
    let mut ids: Vec<String> = (0..51).map(|i| format!("v{i:010}")).collect();
    ids.push(ids[0].clone());
    let mut store = Store::new(ids);
    let client = Client::new(vec![
        response(
            200,
            r#"{"items":[{"id":"v0000000000","status":{"embeddable":true,"privacyStatus":"public"},"snippet":{"title":"test","channelId":"channel"},"contentDetails":{"duration":"PT3M"}}]}"#,
        ),
        response(200, r#"{"items":[]}"#),
    ]);
    let report = block_on(refresh_due(&mut store, &client, NOW)).unwrap();
    assert_eq!(report.purged_rows, 1);
    assert_eq!(report.requested_ids, 51);
    assert_eq!(report.batches, 2);
    assert_eq!(report.updated_rows, 51);
    let calls = client.calls.borrow();
    assert_eq!(calls.iter().map(Vec::len).collect::<Vec<_>>(), [50, 1]);
    let batch = &store.batches[0];
    assert_eq!(batch.validated_at(), NOW);
    assert_eq!(batch.expires_at(), NOW + VALIDATION_TTL_S);
    assert_eq!(batch.videos().len(), 1);
    assert_eq!(batch.videos()[0].video_id, "v0000000000");
    // All 50 requested IDs go to the store, including the 49 absent items.
    assert_eq!(batch.requested_ids().len(), 50);
    assert!(store.batches[1].videos().is_empty());
    assert_eq!(*store.events.borrow(), ["purge", "due", "apply", "apply"]);
}

#[test]
fn api_failures_never_write_validation_and_cleanup_runs_first() {
    for failure in [
        response(403, r#"{"error":{"message":"do not log this"}}"#),
        response(429, r#"{"items":[]}"#),
        response(500, r#"{"items":[]}"#),
        response(302, r#"{"items":[]}"#),
        Err(ValidationError::Transport),
    ] {
        let mut store = Store::new(vec!["dQw4w9WgXcQ".into()]);
        let client = Client::new(vec![failure]);
        let err = block_on(refresh_due(&mut store, &client, NOW)).unwrap_err();
        assert!(matches!(
            err,
            ValidationError::HttpStatus(_) | ValidationError::Transport
        ));
        assert!(!store.expired_metadata);
        assert!(store.batches.is_empty());
        assert_eq!(client.calls.borrow().len(), 1, "no in-run retries");
        assert!(!format!("{err:?}").contains("do not log this"));
    }
}

#[test]
fn malformed_success_does_not_mark_missing_or_fresh() {
    for body in [
        "not json",
        "{}",
        r#"{"error":{"code":403},"items":[]}"#,
        r#"{"items":null}"#,
        r#"{"items":[{"id":"9bZkp7q19f0"}]}"#,
        r#"{"items":[{"id":"dQw4w9WgXcQ"},{"id":"dQw4w9WgXcQ"}]}"#,
        r#"{"items":[{"id":"bad"}]}"#,
    ] {
        let mut store = Store::new(vec!["dQw4w9WgXcQ".into()]);
        let client = Client::new(vec![response(200, body)]);
        assert_eq!(
            block_on(refresh_due(&mut store, &client, NOW)),
            Err(ValidationError::InvalidResponse),
            "{body}"
        );
        assert!(store.batches.is_empty());
    }
}

#[test]
fn oversized_response_does_not_reach_the_store() {
    let mut store = Store::new(vec!["dQw4w9WgXcQ".into()]);
    let client = Client::new(vec![response(200, &" ".repeat(256 * 1024 + 1))]);
    assert_eq!(
        block_on(refresh_due(&mut store, &client, NOW)),
        Err(ValidationError::InvalidResponse)
    );
    assert!(store.batches.is_empty());
}

#[test]
fn rejects_unbounded_or_invalid_queues_without_network() {
    for ids in [
        vec!["bad".into()],
        (0..=MAX_VALIDATIONS_PER_RUN)
            .map(|i| format!("v{i:010}"))
            .collect(),
    ] {
        let mut store = Store::new(ids);
        let client = Client::new(Vec::new());
        assert_eq!(
            block_on(refresh_due(&mut store, &client, NOW)),
            Err(ValidationError::InvalidQueue)
        );
        assert!(client.calls.borrow().is_empty());
        assert!(store.batches.is_empty());
    }
}

#[test]
fn failed_cleanup_prevents_validation_and_failed_commit_is_not_retried() {
    let mut store = Store::new(vec!["dQw4w9WgXcQ".into()]);
    store.fail_cleanup = true;
    let client = Client::new(Vec::new());
    assert_eq!(
        block_on(refresh_due(&mut store, &client, NOW)),
        Err(ValidationError::Store)
    );
    assert!(client.calls.borrow().is_empty());
    assert_eq!(*store.events.borrow(), ["purge"]);

    store.fail_cleanup = false;
    store.fail_commit = true;
    let client = Client::new(vec![response(200, r#"{"items":[]}"#)]);
    assert_eq!(
        block_on(refresh_due(&mut store, &client, NOW)),
        Err(ValidationError::Store)
    );
    assert_eq!(client.calls.borrow().len(), 1);
    assert!(store.batches.is_empty());
}

#[test]
fn completed_batches_survive_a_later_failure_without_replaying_them() {
    let mut store = Store::new((0..51).map(|i| format!("v{i:010}")).collect());
    let client = Client::new(vec![
        response(200, r#"{"items":[]}"#),
        response(503, r#"{"items":[]}"#),
    ]);
    assert_eq!(
        block_on(refresh_due(&mut store, &client, NOW)),
        Err(ValidationError::HttpStatus(503))
    );
    assert_eq!(store.batches.len(), 1);
    assert_eq!(store.batches[0].requested_ids().len(), 50);
    assert_eq!(client.calls.borrow().len(), 2);
}

#[test]
fn empty_queue_only_runs_cleanup_and_invalid_time_does_nothing() {
    let mut store = Store::new(Vec::new());
    let client = Client::new(Vec::new());
    let report = block_on(refresh_due(&mut store, &client, NOW)).unwrap();
    assert_eq!(
        (report.requested_ids, report.batches, report.updated_rows),
        (0, 0, 0)
    );
    assert_eq!(*store.events.borrow(), ["purge", "due"]);
    for now in [-1, 0, i64::MAX] {
        assert_eq!(
            block_on(refresh_due(&mut store, &client, now)),
            Err(ValidationError::InvalidTime)
        );
    }
    assert_eq!(*store.events.borrow(), ["purge", "due"]);
    assert!(client.calls.borrow().is_empty());
}
