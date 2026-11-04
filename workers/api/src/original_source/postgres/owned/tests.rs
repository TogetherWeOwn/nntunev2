//! Native tests for the request-owned driver. Everything is a generated fake
//! with counters; futures are polled manually with a no-op waker. No database,
//! network, spawned task or timer is involved.

use std::{
    cell::{Cell, RefCell},
    future::Future,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll, Waker},
};

use super::*;

type Outcome = Option<Result<(), SourceServiceError>>;

/// Shared observation point for the fake driver, repository and close hook.
struct Probe {
    events: RefCell<Vec<&'static str>>,
    driver_polls: Cell<usize>,
    closes: Cell<usize>,
    /// `None` keeps the driver pending forever.
    driver_outcome: Cell<Outcome>,
    driver_dropped: Cell<bool>,
    repo_dropped: Cell<bool>,
    closes_at_driver_drop: Cell<usize>,
    closes_at_repo_drop: Cell<usize>,
    /// While closed, fake queries stay pending.
    gate_open: Cell<bool>,
    query_error: Cell<Option<SourceServiceError>>,
}

impl Probe {
    fn new() -> Rc<Self> {
        Rc::new(Self {
            events: RefCell::new(Vec::new()),
            driver_polls: Cell::new(0),
            closes: Cell::new(0),
            driver_outcome: Cell::new(None),
            driver_dropped: Cell::new(false),
            repo_dropped: Cell::new(false),
            closes_at_driver_drop: Cell::new(usize::MAX),
            closes_at_repo_drop: Cell::new(usize::MAX),
            gate_open: Cell::new(true),
            query_error: Cell::new(None),
        })
    }

    fn events(&self) -> Vec<&'static str> {
        self.events.borrow().clone()
    }
}

struct FakeDriver(Rc<Probe>);

impl Future for FakeDriver {
    type Output = Result<(), SourceServiceError>;

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        self.0.events.borrow_mut().push("driver");
        self.0.driver_polls.set(self.0.driver_polls.get() + 1);
        match self.0.driver_outcome.get() {
            None => Poll::Pending,
            Some(result) => Poll::Ready(result),
        }
    }
}

impl Drop for FakeDriver {
    fn drop(&mut self) {
        self.0.driver_dropped.set(true);
        self.0.closes_at_driver_drop.set(self.0.closes.get());
    }
}

struct Gate(Rc<Probe>);

impl Future for Gate {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
        if self.0.gate_open.get() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

struct FakeRepo(Rc<Probe>);

impl Drop for FakeRepo {
    fn drop(&mut self) {
        self.0.repo_dropped.set(true);
        self.0.closes_at_repo_drop.set(self.0.closes.get());
    }
}

fn metadata_row() -> SourceMetadata {
    SourceMetadata {
        track_id: "track".to_owned(),
        source_job_id: "job".to_owned(),
        envelope_version: 2,
        payload_format: 1,
        kek_version: 7,
        ciphertext_len: 4,
        wrap_nonce_len: 12,
        wrapped_dek_len: 48,
        payload_nonce_len: 12,
    }
}

impl AsyncSourceRepository for FakeRepo {
    async fn metadata(
        &mut self,
        _: &str,
    ) -> Result<[Option<SourceMetadata>; 2], SourceServiceError> {
        self.0.events.borrow_mut().push("metadata");
        Gate(self.0.clone()).await;
        match self.0.query_error.get() {
            Some(error) => Err(error),
            None => Ok([Some(metadata_row()), None]),
        }
    }

    async fn envelope(
        &mut self,
        _: &SourceMetadata,
    ) -> Result<Option<FieldEnvelope>, SourceServiceError> {
        self.0.events.borrow_mut().push("envelope");
        Gate(self.0.clone()).await;
        match self.0.query_error.get() {
            Some(error) => Err(error),
            None => Ok(Some(FieldEnvelope {
                dek_wrapped_nonce: [1; 12],
                dek_wrapped: vec![2; 48],
                payload_nonce: [3; 12],
                ciphertext: vec![4; 4],
            })),
        }
    }
}

type Repo = DrivenRepository<FakeRepo, FakeDriver, Box<dyn Fn()>>;

fn repo(probe: &Rc<Probe>) -> Repo {
    let counter = probe.clone();
    DrivenRepository::new(
        FakeRepo(probe.clone()),
        FakeDriver(probe.clone()),
        Box::new(move || counter.closes.set(counter.closes.get() + 1)),
    )
}

fn poll_once<F: Future>(future: &mut Pin<Box<F>>) -> Poll<F::Output> {
    future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
}

fn ready<T>(poll: Poll<T>) -> T {
    match poll {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("future unexpectedly pending"),
    }
}

#[test]
fn success_reuses_one_driver_for_metadata_and_envelope() {
    let probe = Probe::new();
    let mut repository = repo(&probe);

    let rows = ready(poll_once(&mut Box::pin(repository.metadata("track")))).unwrap();
    let selected = rows[0].clone().unwrap();
    assert_eq!(selected.track_id, "track");
    assert!(rows[1].is_none());
    let envelope = ready(poll_once(&mut Box::pin(repository.envelope(&selected))))
        .unwrap()
        .unwrap();
    assert_eq!(envelope.ciphertext, vec![4; 4]);

    // The driver is polled before each query can run.
    assert_eq!(probe.events(), ["driver", "metadata", "driver", "envelope"]);
    assert_eq!(probe.driver_polls.get(), 2);
    assert_eq!(probe.closes.get(), 0);
    assert!(!probe.driver_dropped.get());

    drop(repository);
    assert_eq!(probe.closes.get(), 1);
    assert!(probe.driver_dropped.get());
    assert!(probe.repo_dropped.get());
}

#[test]
fn idle_repository_never_polls_the_driver() {
    let probe = Probe::new();
    let repository = repo(&probe);
    assert_eq!(probe.driver_polls.get(), 0);
    assert!(probe.events().is_empty());
    assert_eq!(probe.closes.get(), 0);
    drop(repository);
    assert_eq!(probe.driver_polls.get(), 0);
    assert!(probe.events().is_empty());
    assert_eq!(probe.closes.get(), 1);
}

#[test]
fn driver_completion_wins_a_ready_query_and_closes_once() {
    for outcome in [Ok(()), Err(SourceServiceError::Crypto)] {
        let probe = Probe::new();
        probe.driver_outcome.set(Some(outcome));
        let mut repository = repo(&probe);

        let result = ready(poll_once(&mut Box::pin(repository.metadata("track"))));
        assert!(matches!(result, Err(SourceServiceError::Store)));
        // The ready query was never polled.
        assert_eq!(probe.events(), ["driver"]);
        assert_eq!(probe.closes.get(), 1);

        drop(repository);
        assert_eq!(probe.closes.get(), 1);
    }
}

#[test]
fn driver_completion_fails_a_pending_envelope_query() {
    let probe = Probe::new();
    let mut repository = repo(&probe);
    let selected = metadata_row();
    probe.gate_open.set(false);
    let mut envelope = Box::pin(repository.envelope(&selected));
    assert!(poll_once(&mut envelope).is_pending());
    assert_eq!(probe.closes.get(), 0);

    probe.driver_outcome.set(Some(Ok(())));
    let result = ready(poll_once(&mut envelope));
    assert!(matches!(result, Err(SourceServiceError::Store)));
    assert_eq!(probe.closes.get(), 1);
    drop(envelope);
    drop(repository);
    assert_eq!(probe.closes.get(), 1);
}

#[test]
fn query_error_poisons_and_closes_once() {
    let probe = Probe::new();
    probe.query_error.set(Some(SourceServiceError::Denied));
    let mut repository = repo(&probe);

    let result = ready(poll_once(&mut Box::pin(repository.metadata("track"))));
    assert!(matches!(result, Err(SourceServiceError::Denied)));
    assert_eq!(probe.closes.get(), 1);
    assert_eq!(probe.events(), ["driver", "metadata"]);

    drop(repository);
    assert_eq!(probe.closes.get(), 1);
}

#[test]
fn query_cancellation_closes_immediately() {
    let probe = Probe::new();
    probe.gate_open.set(false);
    let mut repository = repo(&probe);

    let mut query = Box::pin(repository.metadata("track"));
    assert!(poll_once(&mut query).is_pending());
    assert_eq!(probe.closes.get(), 0);
    drop(query);
    // Closed while the repository is still alive.
    assert_eq!(probe.closes.get(), 1);
    assert!(!probe.driver_dropped.get());
    assert!(!probe.repo_dropped.get());

    drop(repository);
    assert_eq!(probe.closes.get(), 1);
}

#[test]
fn poisoned_repository_never_polls_new_queries_or_driver() {
    let probe = Probe::new();
    probe.query_error.set(Some(SourceServiceError::Invalid));
    let mut repository = repo(&probe);
    assert!(ready(poll_once(&mut Box::pin(repository.metadata("track")))).is_err());
    let events = probe.events();
    let polls = probe.driver_polls.get();

    // Even with a healthy store afterwards, the poisoned repository refuses.
    probe.query_error.set(None);
    let selected = metadata_row();
    let result = ready(poll_once(&mut Box::pin(repository.metadata("track"))));
    assert!(matches!(result, Err(SourceServiceError::Store)));
    let result = ready(poll_once(&mut Box::pin(repository.envelope(&selected))));
    assert!(matches!(result, Err(SourceServiceError::Store)));

    assert_eq!(probe.events(), events);
    assert_eq!(probe.driver_polls.get(), polls);
    assert_eq!(probe.closes.get(), 1);
    drop(repository);
    assert_eq!(probe.closes.get(), 1);
}

#[test]
fn dropping_an_unpolled_repository_drops_parts_and_closes_once() {
    let probe = Probe::new();
    drop(repo(&probe));
    assert_eq!(probe.closes.get(), 1);
    assert!(probe.driver_dropped.get());
    assert!(probe.repo_dropped.get());
    // Closure was initiated before either field was destroyed.
    assert_eq!(probe.closes_at_driver_drop.get(), 1);
    assert_eq!(probe.closes_at_repo_drop.get(), 1);
    assert_eq!(probe.driver_polls.get(), 0);
    assert!(probe.events().is_empty());
}

#[test]
fn dropping_a_repository_with_a_pending_query_closes_once() {
    let probe = Probe::new();
    probe.gate_open.set(false);
    let mut repository = repo(&probe);
    let mut query = Box::pin(repository.metadata("track"));
    assert!(poll_once(&mut query).is_pending());
    // The borrow forces the pending query to go first, then the repository.
    drop(query);
    assert_eq!(probe.closes.get(), 1);
    drop(repository);
    assert_eq!(probe.closes.get(), 1);
    assert!(probe.driver_dropped.get());
    assert!(probe.repo_dropped.get());
    assert_eq!(probe.closes_at_driver_drop.get(), 1);
    assert_eq!(probe.closes_at_repo_drop.get(), 1);
}

#[test]
fn pending_envelope_cancellation_closes_and_poisons() {
    let probe = Probe::new();
    let mut repository = repo(&probe);
    let rows = ready(poll_once(&mut Box::pin(repository.metadata("track")))).unwrap();
    let selected = rows[0].clone().unwrap();
    assert_eq!(probe.closes.get(), 0);

    probe.gate_open.set(false);
    let mut envelope = Box::pin(repository.envelope(&selected));
    assert!(poll_once(&mut envelope).is_pending());
    assert_eq!(probe.closes.get(), 0);
    drop(envelope);
    assert_eq!(probe.closes.get(), 1);

    probe.gate_open.set(true);
    let polls = probe.driver_polls.get();
    let result = ready(poll_once(&mut Box::pin(repository.metadata("track"))));
    assert!(matches!(result, Err(SourceServiceError::Store)));
    assert_eq!(probe.driver_polls.get(), polls);
    drop(repository);
    assert_eq!(probe.closes.get(), 1);
}

#[test]
fn pending_query_that_later_completes_keeps_the_connection_open() {
    let probe = Probe::new();
    probe.gate_open.set(false);
    let mut repository = repo(&probe);
    let mut query = Box::pin(repository.metadata("track"));
    assert!(poll_once(&mut query).is_pending());
    assert!(poll_once(&mut query).is_pending());
    assert_eq!(probe.driver_polls.get(), 2);

    probe.gate_open.set(true);
    assert!(ready(poll_once(&mut query)).is_ok());
    drop(query);
    assert_eq!(probe.closes.get(), 0);
    drop(repository);
    assert_eq!(probe.closes.get(), 1);
}
