//! Native tests for bounded request I/O. The fakes model the Worker adapter
//! contract only; the source-shape tests at the bottom do NOT prove Worker
//! execution, only that the adapter text keeps its reviewed structure.

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    future::{pending, Future},
    pin::Pin,
    rc::Rc,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    task::{Context, Poll, Wake, Waker},
};

use super::*;

const JSON: &str = "application/json";

fn head<'a>(
    content_type: Option<&'a str>,
    content_length: Option<&'a str>,
    content_encoding: Option<&'a str>,
) -> Head<'a> {
    Head {
        content_type,
        content_length,
        content_encoding,
    }
}

#[derive(Default)]
struct Probe {
    nexts: Cell<usize>,
    remainings: RefCell<Vec<usize>>,
    body_drops: Cell<usize>,
    work_polls: Cell<usize>,
    work_drops: Cell<usize>,
}

enum Step {
    Chunk(Vec<u8>),
    Err,
    Pending,
}

/// An exhausted script is end of stream.
struct Fake {
    steps: VecDeque<Step>,
    probe: Rc<Probe>,
}

impl Fake {
    fn new(probe: &Rc<Probe>, steps: Vec<Step>) -> Self {
        Self {
            steps: steps.into(),
            probe: probe.clone(),
        }
    }
}

impl Body for Fake {
    async fn next(&mut self, remaining: usize) -> Result<Option<Vec<u8>>, RegistryError> {
        self.probe.nexts.set(self.probe.nexts.get() + 1);
        self.probe.remainings.borrow_mut().push(remaining);
        match self.steps.pop_front() {
            Some(Step::Chunk(bytes)) => Ok(Some(bytes)),
            Some(Step::Err) => Err(RegistryError),
            Some(Step::Pending) => pending().await,
            None => Ok(None),
        }
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.probe.body_drops.set(self.probe.body_drops.get() + 1);
    }
}

struct WorkGuard(Rc<Probe>);
impl Drop for WorkGuard {
    fn drop(&mut self) {
        self.0.work_drops.set(self.0.work_drops.get() + 1);
    }
}

#[derive(Default)]
struct SignalState {
    fired: Cell<bool>,
    waker: RefCell<Option<Waker>>,
}

/// Portable stand-in for the Worker abort adapter: it stores the most recent
/// waker, like `RequestAbort`, and wakes only that one when fired.
#[derive(Clone, Default)]
struct Signal(Rc<SignalState>);

impl Signal {
    fn fire(&self) {
        self.0.fired.set(true);
        if let Some(waker) = self.0.waker.borrow_mut().take() {
            waker.wake();
        }
    }
}

impl Future for Signal {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0.fired.get() {
            return Poll::Ready(());
        }
        *self.0.waker.borrow_mut() = Some(cx.waker().clone());
        Poll::Pending
    }
}

#[derive(Default)]
struct Counter(AtomicUsize);
impl Wake for Counter {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn counting() -> (Arc<Counter>, Waker) {
    let counter = Arc::new(Counter::default());
    (counter.clone(), Waker::from(counter))
}

fn poll_with<F: Future>(fut: &mut Pin<Box<F>>, waker: &Waker) -> Poll<F::Output> {
    fut.as_mut().poll(&mut Context::from_waker(waker))
}

async fn collected(
    steps: Vec<Step>,
    cap: usize,
    declared: Option<usize>,
) -> (Result<Vec<u8>, RegistryError>, Rc<Probe>) {
    let probe = Rc::new(Probe::default());
    let result = collect(Fake::new(&probe, steps), cap, declared).await;
    (result, probe)
}

// ---- Head ----

#[test]
fn head_rejects_bad_content_type_and_encoding() {
    for bad in [
        head(None, None, None),
        head(Some("text/plain"), None, None),
        head(Some("application/json; charset=utf-8"), None, None),
        head(Some("Application/JSON"), None, None),
        head(Some(JSON), None, Some("gzip")),
        head(Some(JSON), None, Some("identity")),
        head(Some(JSON), Some("2"), Some("")),
    ] {
        assert_eq!(bad.validate(16), Err(RegistryError));
    }
}

#[test]
fn head_rejects_malformed_or_oversized_length() {
    for raw in [
        "",
        "-1",
        "+1",
        " 1",
        "1 ",
        "1a",
        "0x10",
        "1.0",
        "17",
        "99999999999",
        "٣",
    ] {
        assert_eq!(
            head(Some(JSON), Some(raw), None).validate(16),
            Err(RegistryError),
            "{raw:?}"
        );
    }
}

#[test]
fn head_accepts_absent_zero_and_exact_cap() {
    assert_eq!(head(Some(JSON), None, None).validate(16), Ok(None));
    assert_eq!(head(Some(JSON), Some("0"), None).validate(16), Ok(Some(0)));
    assert_eq!(
        head(Some(JSON), Some("16"), None).validate(16),
        Ok(Some(16))
    );
}

// ---- collect ----

#[tokio::test]
async fn collect_accepts_exact_declared_and_exact_cap() {
    let (result, probe) = collected(
        vec![Step::Chunk(vec![1, 2]), Step::Chunk(vec![3, 4])],
        4,
        Some(4),
    )
    .await;
    assert_eq!(result, Ok(vec![1, 2, 3, 4]));
    assert_eq!(*probe.remainings.borrow(), [4, 2, 0]);
    assert_eq!(probe.body_drops.get(), 1);
    let (result, _) = collected(vec![Step::Chunk(vec![9; 4])], 4, None).await;
    assert_eq!(result, Ok(vec![9; 4]));
}

#[tokio::test]
async fn collect_accepts_undeclared_eof_under_cap_and_empty() {
    let (result, _) = collected(vec![Step::Chunk(vec![7])], 4, None).await;
    assert_eq!(result, Ok(vec![7]));
    let (result, _) = collected(vec![], 4, None).await;
    assert_eq!(result, Ok(vec![]));
}

#[tokio::test]
async fn collect_rejects_eof_short_of_declared() {
    let (result, _) = collected(vec![Step::Chunk(vec![1, 2])], 8, Some(3)).await;
    assert_eq!(result, Err(RegistryError));
    let (result, _) = collected(vec![], 8, Some(1)).await;
    assert_eq!(result, Err(RegistryError));
}

#[tokio::test]
async fn collect_rejects_body_over_declared_or_cap() {
    // Declared 2 with a third byte: the honest body is refused by the
    // limit-derived remaining, never buffered past the declaration.
    let (result, _) = collected(
        vec![Step::Chunk(vec![1, 2]), Step::Chunk(vec![3])],
        8,
        Some(2),
    )
    .await;
    assert_eq!(result, Err(RegistryError));
    let (result, _) = collected(
        vec![Step::Chunk(vec![1, 2, 3]), Step::Chunk(vec![4, 5])],
        4,
        None,
    )
    .await;
    assert_eq!(result, Err(RegistryError));
}

#[tokio::test]
async fn collect_rejects_declared_over_cap_without_reading() {
    let (result, probe) = collected(vec![Step::Chunk(vec![1])], 4, Some(5)).await;
    assert_eq!(result, Err(RegistryError));
    assert_eq!(probe.nexts.get(), 0);
    assert_eq!(probe.body_drops.get(), 1);
}

#[tokio::test]
async fn collect_rejects_dishonest_body_exceeding_remaining() {
    let (result, probe) = collected(
        vec![
            Step::Chunk(vec![1]),
            Step::Chunk(vec![0; 4]),
            Step::Chunk(vec![9]),
        ],
        4,
        None,
    )
    .await;
    assert_eq!(result, Err(RegistryError));
    assert_eq!(probe.nexts.get(), 2, "stops at the violating chunk");
    assert_eq!(probe.body_drops.get(), 1);
}

#[tokio::test]
async fn collect_rejects_empty_non_eof_chunk() {
    for declared in [None, Some(2)] {
        let (result, probe) = collected(
            vec![
                Step::Chunk(vec![1]),
                Step::Chunk(vec![]),
                Step::Chunk(vec![2]),
            ],
            4,
            declared,
        )
        .await;
        assert_eq!(result, Err(RegistryError));
        assert_eq!(probe.nexts.get(), 2, "stops at the empty chunk");
        assert_eq!(probe.body_drops.get(), 1);
    }
}

#[tokio::test]
async fn collect_propagates_body_error_and_drops_body() {
    let (result, probe) = collected(vec![Step::Chunk(vec![1]), Step::Err], 4, Some(2)).await;
    assert_eq!(result, Err(RegistryError));
    assert_eq!(probe.nexts.get(), 2);
    assert_eq!(probe.body_drops.get(), 1);
}

// ---- run ----

#[tokio::test]
async fn run_returns_work_output_when_nothing_fires() {
    let result = run(async { 7 }, pending(), pending()).await;
    assert_eq!(result, Ok(7));
}

#[tokio::test]
async fn run_cancellation_drops_pending_body() {
    let probe = Rc::new(Probe::default());
    let signal = Signal::default();
    let work = collect(Fake::new(&probe, vec![Step::Pending]), 4, None);
    let (counter, waker) = counting();
    let mut fut = Box::pin(run(work, signal.clone(), pending()));
    assert!(poll_with(&mut fut, &waker).is_pending());
    assert_eq!(probe.nexts.get(), 1);
    assert_eq!(probe.body_drops.get(), 0);
    signal.fire();
    assert_eq!(counter.0.load(Ordering::SeqCst), 1);
    assert!(matches!(
        poll_with(&mut fut, &waker),
        Poll::Ready(Err(RegistryError))
    ));
    assert_eq!(probe.body_drops.get(), 1, "losing work drops the body");
}

#[tokio::test]
async fn run_deadline_drops_pending_body() {
    let probe = Rc::new(Probe::default());
    let deadline = Signal::default();
    let work = collect(Fake::new(&probe, vec![Step::Pending]), 4, None);
    let (_, waker) = counting();
    let mut fut = Box::pin(run(work, pending(), deadline.clone()));
    assert!(poll_with(&mut fut, &waker).is_pending());
    deadline.fire();
    assert!(matches!(
        poll_with(&mut fut, &waker),
        Poll::Ready(Err(RegistryError))
    ));
    assert_eq!(probe.body_drops.get(), 1);
}

#[tokio::test]
async fn run_dropped_future_drops_work_and_body() {
    let probe = Rc::new(Probe::default());
    let guard = WorkGuard(probe.clone());
    let body = Fake::new(&probe, vec![Step::Pending]);
    let work = async move {
        let _guard = guard;
        collect(body, 4, None).await
    };
    let (_, waker) = counting();
    let mut fut = Box::pin(run(work, pending(), pending()));
    assert!(poll_with(&mut fut, &waker).is_pending());
    assert_eq!((probe.work_drops.get(), probe.body_drops.get()), (0, 0));
    drop(fut);
    assert_eq!((probe.work_drops.get(), probe.body_drops.get()), (1, 1));
}

#[tokio::test]
async fn run_ready_cancellation_or_deadline_wins_before_work_dispatch() {
    for (cancelled, expired) in [(true, false), (false, true), (true, true)] {
        let probe = Rc::new(Probe::default());
        let body = Fake::new(&probe, vec![Step::Chunk(vec![1])]);
        let p = probe.clone();
        let work = async move {
            p.work_polls.set(p.work_polls.get() + 1);
            collect(body, 4, None).await
        };
        let ready = async {};
        let result = match (cancelled, expired) {
            (true, false) => run(work, ready, pending()).await,
            (false, true) => run(work, pending(), ready).await,
            _ => run(work, async {}, async {}).await,
        };
        assert!(result.is_err());
        assert_eq!(probe.work_polls.get(), 0);
        assert_eq!(probe.nexts.get(), 0);
        assert_eq!(probe.body_drops.get(), 1, "unstarted work is still dropped");
    }
}

#[tokio::test]
async fn run_replaces_stored_waker_across_polls() {
    let signal = Signal::default();
    let (first, first_waker) = counting();
    let (second, second_waker) = counting();
    let mut fut = Box::pin(run(pending::<()>(), signal.clone(), pending()));
    assert!(poll_with(&mut fut, &first_waker).is_pending());
    assert!(poll_with(&mut fut, &second_waker).is_pending());
    signal.fire();
    assert_eq!(first.0.load(Ordering::SeqCst), 0, "stale waker is replaced");
    assert_eq!(second.0.load(Ordering::SeqCst), 1);
    assert!(matches!(
        poll_with(&mut fut, &second_waker),
        Poll::Ready(Err(RegistryError))
    ));
}

// ---- source-shape regression (does NOT prove Worker execution) ----

const ADAPTER: &str = include_str!("worker.rs");
const RUNTIME: &str = include_str!("../account_runtime.rs");

fn at(haystack: &str, needle: &str) -> usize {
    haystack
        .find(needle)
        .unwrap_or_else(|| panic!("missing {needle:?}"))
}

#[test]
fn adapter_source_checks_raw_length_before_copying() {
    let string = at(ADAPTER, "fn bounded_string");
    let tail = &ADAPTER[string..];
    assert!(at(tail, "value.length() > limit") < at(tail, "as_string()"));
    let body = at(ADAPTER, "impl Body for RequestBody");
    let tail = &ADAPTER[body..];
    assert!(at(tail, "chunk.length() == 0") < at(tail, "chunk.to_vec()"));
    assert!(at(tail, "chunk.length() as usize > remaining") < at(tail, "chunk.to_vec()"));
    assert_eq!(ADAPTER.matches("as_string()").count(), 1);
    assert_eq!(ADAPTER.matches("to_vec()").count(), 1);
}

#[test]
fn adapter_source_catches_get_reader_and_cancels_on_drop() {
    assert!(ADAPTER.contains("&\"getReader\".into()"));
    assert!(!ADAPTER.contains(".get_reader("));
    let call = at(ADAPTER, "get_reader\n        .call0(");
    assert!(call < at(ADAPTER, "super::collect(RequestBody { reader }"));
    assert!(ADAPTER[call..].contains(".map_err(|_| RegistryError)?"));
    let drop = at(ADAPTER, "impl Drop for RequestBody");
    assert!(ADAPTER[drop..].contains("self.reader.cancel()"));
    assert!(!ADAPTER.contains("spawn_local"));
}

#[test]
fn adapter_source_keeps_abort_listener_and_waker_replacement() {
    let add = at(ADAPTER, "add_event_listener_with_callback(\"abort\"");
    assert!(at(ADAPTER, "Closure::new(") < add);
    let poll = at(ADAPTER, "impl Future for RequestAbort");
    let tail = &ADAPTER[poll..];
    assert!(
        at(tail, "self.signal.aborted()")
            < at(tail, "*self.waker.borrow_mut() = Some(cx.waker().clone())")
    );
    assert!(tail.matches("self.signal.aborted()").count() >= 2);
    let drop = at(ADAPTER, "impl Drop for RequestAbort");
    assert!(ADAPTER[drop..].contains("remove_event_listener_with_callback(\"abort\""));
    assert!(ADAPTER[drop..].contains("self.listener.as_ref()"));
}

#[test]
fn runtime_source_wires_run_across_all_handlers() {
    assert_eq!(RUNTIME.matches("RequestAbort::new(").count(), 2);
    assert_eq!(RUNTIME.matches("account_request::run(").count(), 3);
    let fetch = at(RUNTIME, "async fn fetch(&self, mut req: Request)");
    let ingest = at(RUNTIME, "pub(crate) async fn ingest_fetch");
    assert!(fetch < at(RUNTIME, "self.operation(&mut req)"));
    assert!(fetch < at(RUNTIME, "self.candidate(&mut req)"));
    assert!(RUNTIME[ingest..].contains("account_request::run(work, cancellation,"));
    // Request bodies are only read inside the raced work, never up front.
    for (at_read, _) in RUNTIME.match_indices("request_io::read(") {
        assert!(at_read > fetch);
    }
    assert!(!RUNTIME.contains("req.text()") && !RUNTIME.contains("req.bytes()"));
}

#[test]
fn runtime_qualification_gate_stays_intact_before_body_reads() {
    assert_eq!(RUNTIME.matches("const OPERATIONS_QUALIFIED").count(), 1);
    assert!(RUNTIME.contains("const OPERATIONS_QUALIFIED: bool = false;"));
    assert!(!RUNTIME.contains("OPERATIONS_QUALIFIED: bool = true"));
    assert!(RUNTIME.contains("path == OPERATION_PATH && OPERATIONS_QUALIFIED"));
    let ingest = at(RUNTIME, "pub(crate) async fn ingest_fetch");
    let tail = &RUNTIME[ingest..];
    let gate = at(tail, "!OPERATIONS_QUALIFIED");
    assert!(gate < at(tail, "RequestAbort::new("));
    assert!(gate < at(tail, "request_io::read("));
    assert!(gate < at(tail, "env\n            .secret("));
}
