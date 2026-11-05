//! Synthetic cross-boundary proof for the pool-owned response handoff. The
//! REAL `ArticleSupplier::prepare_owned`, `into_response` and `pump_owned`
//! drive the reviewed response library; only the catalogue, registry, account
//! replies, signals and tiny writer are fakes. These are transport boundaries,
//! not Worker scheduling, physical transport closure or delivered-report
//! durability proof. Keys stay in zeroizing owners and are never logged.

use super::*;

use std::rc::Weak;

use super::super::owned::{OwnedCase, Probe, Witness};
use crate::{
    delivered::{DeliveredInterval, DeliveredSink},
    play_route::PlayRouteRefusal,
};

type Supplier = ArticleSupplier<'static, Probe<FakeRegistry>, Probe<FakeAccounts>>;
type Response =
    super::super::super::task::OwnedAccountResponse<Probe<FakeRegistry>, Probe<FakeAccounts>>;
type Task = Pin<Box<dyn Future<Output = Result<(), SourceError>> + 'static>>;
type SharedVec = Rc<RefCell<VecDeliveredSink>>;

// ------------------------------------------------------------ fixtures

/// Counts lookups; the wrapped fake is consumed by `prepare_owned`.
struct CountingCatalogue {
    inner: FakeCatalogue,
    lookups: Rc<Cell<usize>>,
}

impl OriginalCatalogue for CountingCatalogue {
    async fn source(&self, track_id: &str) -> Result<Option<OriginalSource>, CatalogueUnavailable> {
        self.lookups.set(self.lookups.get() + 1);
        self.inner.source(track_id).await
    }
}

/// Collects delivered intervals into state the test keeps after the sink has
/// moved into the static task.
struct SharedSink(SharedVec);

impl DeliveredSink for SharedSink {
    fn delivered(&mut self, interval: DeliveredInterval) {
        self.0.borrow_mut().delivered(interval);
    }
}

#[derive(Default)]
struct Counts {
    lookups: Rc<Cell<usize>>,
    registry: Rc<Cell<usize>>,
    hints: Rc<Cell<usize>>,
    bodies: Rc<Cell<usize>>,
    /// Article indices returned by the fake BODY endpoint, in dispatch order.
    served: Rc<RefCell<Vec<usize>>>,
}

struct Harness {
    supplier: Option<Supplier>,
    observer: RequestControl<'static>,
    witness: Witness,
    counts: Counts,
    hint_dropped: Rc<Cell<bool>>,
    source: Weak<OriginalSource>,
}

impl Harness {
    /// (lookups, registry, hints, bodies)
    fn io(&self) -> (usize, usize, usize, usize) {
        (
            self.counts.lookups.get(),
            self.counts.registry.get(),
            self.counts.hints.get(),
            self.counts.bodies.get(),
        )
    }

    fn into_response(&mut self, range: Option<&str>) -> Result<Response, PlayRouteRefusal> {
        let authorization =
            authorize_original(&play_request(range), GRANT_KEY, NOW / 1000).unwrap();
        self.supplier
            .take()
            .unwrap()
            .into_response(authorization, range)
    }

    /// Drops the request authority clone and proves every owned input and the
    /// shared source allocation were released.
    fn assert_released(self) {
        let Self {
            observer,
            witness,
            source,
            ..
        } = self;
        drop(observer);
        witness.assert_released();
        assert!(source.upgrade().is_none());
    }
}

/// Two accounts so the hint stage is reached. Real owned preparation runs here
/// (one lookup); no article, registry or hint I/O happens yet.
fn harness(parts: usize, fail_on: Option<usize>, tweak: impl FnOnce(&Rig)) -> Harness {
    let rig = Rig::with_rows(parts, &[(1, 0), (2, 1)]);
    let fixture = generated(parts);
    *rig.catalogue.mode.borrow_mut() = CatMode::Source(fixture.source.clone());
    let counts = Counts::default();
    let inner = article_reply(fixture.bodies.clone(), fail_on);
    let served = counts.served.clone();
    *rig.accounts.reply.borrow_mut() = Box::new(move |account, body| {
        let reply = inner(account, body);
        served.borrow_mut().push(requested_index(body));
        reply
    });
    for (hook, counter) in [
        (&rig.registry.hook, counts.registry.clone()),
        (&rig.accounts.candidate_hook, counts.hints.clone()),
        (&rig.accounts.execute_hook, counts.bodies.clone()),
    ] {
        set_hook(hook, move |_| counter.set(counter.get() + 1));
    }
    tweak(&rig);
    let hint_dropped = rig.accounts.candidate_dropped.clone();
    let OwnedCase {
        catalogue,
        inputs,
        control,
        witness,
    } = OwnedCase::new(rig);
    let observer = control.clone();
    let catalogue = CountingCatalogue {
        inner: catalogue,
        lookups: counts.lookups.clone(),
    };
    let supplier = ready(ArticleSupplier::prepare_owned(catalogue, inputs, control)).unwrap();
    let source = Rc::downgrade(&supplier.shared_source());
    Harness {
        supplier: Some(supplier),
        observer,
        witness,
        counts,
        hint_dropped,
        source,
    }
}

/// The returned pump is wrapped in an outer `'static` task BEFORE any poll.
fn launch(response: Response, sink: &SharedVec, writer: &WriterRef) -> Task {
    let pump = response.pump_owned(SharedSink(sink.clone()), Writer(writer.clone()));
    Box::pin(async move { pump.await })
}

fn step(task: &mut Task, waker: &Waker) -> Poll<Result<(), SourceError>> {
    task.as_mut().poll(&mut Context::from_waker(waker))
}

fn drive_task(task: &mut Task) -> Result<(), SourceError> {
    for _ in 0..64 {
        if let Poll::Ready(result) = step(task, Waker::noop()) {
            return result;
        }
    }
    panic!("response did not finish");
}

// ---------------------------------------------------- metadata and refusal

#[test]
fn into_response_shares_one_source_allocation_and_does_no_io() {
    let cases = [
        (None, 200, 0u64..12),
        (Some("bytes=2-8"), 206, 2..9),
        (Some("bytes=-3"), 206, 9..12),
    ];
    for (range, status, bytes) in cases {
        let mut h = harness(4, None, |_| {});
        assert_eq!(h.source.strong_count(), 1);
        assert_eq!(h.io(), (1, 0, 0, 0));
        let response = h.into_response(range).unwrap();
        let length = bytes.end - bytes.start;
        assert_eq!((response.status(), response.body_len()), (status, length));
        assert!(response
            .headers()
            .contains(&("content-length".to_owned(), length.to_string())));
        assert_eq!(
            response.headers().contains(&(
                "content-range".to_owned(),
                format!("bytes {}-{}/{FILE_BYTES}", bytes.start, bytes.end - 1)
            )),
            status == 206,
            "{range:?}"
        );
        // Supplier and plan share the exact allocation: no deep source clone,
        // no second lookup and no article I/O for metadata.
        assert_eq!(h.source.strong_count(), 2, "{range:?}");
        assert_eq!(h.io(), (1, 0, 0, 0), "{range:?}");
        drop(response);
        assert_eq!(h.io(), (1, 0, 0, 0), "{range:?}");
        h.assert_released();
    }
}

#[test]
fn refused_range_consumes_the_supplier_and_releases_everything_without_io() {
    let mut h = harness(4, None, |_| {});
    let refusal = match h.into_response(Some("bytes=99-120")) {
        Err(refusal) => refusal,
        Ok(_) => panic!("range must be refused"),
    };
    assert_eq!(refusal.status, 416);
    assert_eq!(h.io(), (1, 0, 0, 0));
    assert!(!h.witness.operation_timer.dropped.get());
    h.assert_released();
}

// ----------------------------------------------------------- full streams

#[test]
fn static_outer_task_streams_full_and_ranged_bodies_through_the_real_pump() {
    // (range, expected file bytes, expected BODY order)
    let cases = [
        (None, 0..12u8, vec![3, 0, 1, 2]),
        (Some("bytes=2-8"), 2..9, vec![3, 0, 1, 2]),
        (Some("bytes=0-5"), 0..6, vec![3, 0, 1]),
        (Some("bytes=-3"), 9..12, vec![3, 0, 1, 2]),
    ];
    for (range, expected, opens) in cases {
        let mut h = harness(4, None, |_| {});
        let response = h.into_response(range).unwrap();
        let sink = SharedVec::default();
        let writer = WriterRef::default();
        let mut task = launch(response, &sink, &writer);
        assert_eq!(h.io(), (1, 0, 0, 0), "nothing opens before the first poll");
        assert_eq!(drive_task(&mut task), Ok(()), "{range:?}");
        drop(task);

        {
            let state = writer.borrow();
            assert_eq!(
                state.got,
                expected.clone().collect::<Vec<u8>>(),
                "{range:?}"
            );
            assert_eq!((state.closes, state.aborts), (1, 0), "{range:?}");
        }
        // One lookup, one BODY per needed article, no retry and no drain.
        assert_eq!(*h.counts.served.borrow(), opens, "{range:?}");
        let (lookups, registry, _, bodies) = h.io();
        assert_eq!((lookups, registry, bodies), (1, opens.len(), opens.len()));
        // One interval for the acknowledged range.
        assert_interval(
            &sink.borrow(),
            u64::from(expected.start),
            u64::from(expected.end),
        );
        assert!(live(&h.observer), "{range:?}");
        h.assert_released();
    }
}

#[test]
fn failed_article_keeps_only_the_acknowledged_prefix_without_retry() {
    // (failing BODY number, acknowledged bytes before it)
    for (fail_on, acked) in [(1usize, 0u64), (3, 3)] {
        let mut h = harness(4, Some(fail_on), |_| {});
        let response = h.into_response(None).unwrap();
        let sink = SharedVec::default();
        let writer = WriterRef::default();
        let mut task = launch(response, &sink, &writer);
        assert_eq!(
            drive_task(&mut task),
            Err(SourceError::Transport),
            "fail on {fail_on}"
        );
        drop(task);

        assert_eq!(h.counts.bodies.get(), fail_on, "no retry or failover");
        assert_eq!(h.counts.lookups.get(), 1);
        assert!(poisoned(&h.observer));
        {
            let state = writer.borrow();
            assert_eq!(state.got.len() as u64, acked);
            assert_eq!((state.closes, state.aborts), (0, 1));
        }
        if acked == 0 {
            assert!(sink.borrow().0.is_empty());
        } else {
            assert_interval(&sink.borrow(), 0, acked);
        }
        h.assert_released();
    }
}

// ------------------------------------------------------ unpolled and drop

#[test]
fn unpolled_pump_or_outer_task_drop_aborts_the_writer_once_with_zero_io() {
    for wrapped in [false, true] {
        let mut h = harness(4, None, |_| {});
        let response = h.into_response(None).unwrap();
        let sink = SharedVec::default();
        let writer = WriterRef::default();
        if wrapped {
            drop(launch(response, &sink, &writer));
        } else {
            drop(response.pump_owned(SharedSink(sink.clone()), Writer(writer.clone())));
        }
        {
            let state = writer.borrow();
            assert_eq!(
                (state.aborts, state.closes, state.write_polls),
                (1, 0, 0),
                "wrapped {wrapped}"
            );
            assert!(state.got.is_empty());
        }
        assert!(sink.borrow().0.is_empty(), "zero delivery");
        assert_eq!(h.io(), (1, 0, 0, 0), "no article, registry or hint I/O");
        assert!(!h.witness.operation_timer.dropped.get());
        h.assert_released();
    }
}

#[test]
fn same_authority_stop_before_first_poll_does_no_io_and_delivers_nothing() {
    let mut h = harness(4, None, |_| {});
    let response = h.into_response(None).unwrap();
    let sink = SharedVec::default();
    let writer = WriterRef::default();
    let mut task = launch(response, &sink, &writer);
    h.observer.stop();
    assert_eq!(
        step(&mut task, Waker::noop()),
        Poll::Ready(Err(SourceError::Cancelled))
    );
    drop(task);
    assert_eq!(h.io(), (1, 0, 0, 0));
    assert!(sink.borrow().0.is_empty());
    let state = writer.borrow();
    assert_eq!((state.aborts, state.closes, state.write_polls), (1, 0, 0));
    drop(state);
    h.assert_released();
}

// ------------------------------------------------- pending stage and cause

#[derive(Clone, Copy, Debug)]
enum Stage {
    Registry,
    Hint,
    Body,
    Writer,
    Close,
}

#[derive(Clone, Copy, Debug)]
enum Cause {
    Drop,
    Stop,
    Abort,
    RequestTimer,
    OperationTimer,
    Clock,
}

fn reached(stage: Stage, h: &Harness, writer: &WriterRef) -> bool {
    let (_, registry, hints, bodies) = h.io();
    let state = writer.borrow();
    match stage {
        Stage::Registry => registry > 0,
        Stage::Hint => hints > 0,
        Stage::Body => bodies > 0,
        Stage::Writer => state.got.len() == PART_BYTES && bodies == 3,
        Stage::Close => state.closes > 0 && state.got.len() == FILE_BYTES as usize,
    }
}

#[test]
fn pending_stage_drop_or_same_authority_cancel_releases_everything_with_acked_prefix_only() {
    let stages = [
        Stage::Registry,
        Stage::Hint,
        Stage::Body,
        Stage::Writer,
        Stage::Close,
    ];
    let causes = [
        Cause::Drop,
        Cause::Stop,
        Cause::Abort,
        Cause::RequestTimer,
        Cause::OperationTimer,
        Cause::Clock,
    ];
    for stage in stages {
        let writer_stage = matches!(stage, Stage::Writer | Stage::Close);
        for cause in causes {
            // The per-open operation timer is gone once the BODY is returned.
            if writer_stage && matches!(cause, Cause::OperationTimer) {
                continue;
            }
            let label = format!("{stage:?} {cause:?}");
            let mut h = harness(4, None, |rig| match stage {
                Stage::Registry => rig.registry.pending.set(true),
                Stage::Hint => rig.accounts.candidate_pending.set(true),
                Stage::Body => rig.accounts.execute_pending.set(true),
                Stage::Writer | Stage::Close => {}
            });
            let writer = WriterRef::default();
            match stage {
                Stage::Writer => writer.borrow_mut().block_after = Some(PART_BYTES),
                Stage::Close => writer.borrow_mut().pending_close = true,
                _ => {}
            }
            let flag = match stage {
                Stage::Registry => h.witness.pending_registry.clone(),
                Stage::Hint => h.hint_dropped.clone(),
                Stage::Body => h.witness.pending_body.clone(),
                Stage::Writer | Stage::Close => Rc::default(),
            };
            let response = h.into_response(None).unwrap();
            let sink = SharedVec::default();
            let mut task = launch(response, &sink, &writer);
            let wakes = Arc::new(Wakes::default());
            let waker = Waker::from(wakes.clone());

            let mut parked = false;
            for _ in 0..64 {
                if step(&mut task, &waker).is_pending() && reached(stage, &h, &writer) {
                    parked = true;
                    break;
                }
            }
            assert!(parked, "{label}");
            assert!(!flag.get(), "{label}");
            let io = h.io();
            let (polls, closes, acked) = {
                let state = writer.borrow();
                (state.write_polls, state.closes, state.got.len())
            };

            match cause {
                Cause::Drop => drop(task),
                _ => {
                    let before_wake = wakes.0.load(Ordering::Relaxed);
                    match cause {
                        Cause::Stop => h.observer.stop(),
                        Cause::Abort => h.witness.abort.fire(),
                        Cause::RequestTimer => h.witness.request_timer.fire(),
                        Cause::Clock => h.witness.clock.upgrade().unwrap().set(GRANT_EXP_MS),
                        _ => h.witness.operation_timer.fire(),
                    }
                    // A clock jump has no external wake; the next poll observes
                    // expiry. Wakeable causes must advance past prior yields.
                    if !matches!(cause, Cause::Clock) {
                        assert!(wakes.0.load(Ordering::Relaxed) > before_wake, "{label}");
                    }
                    assert_eq!(
                        step(&mut task, &waker),
                        Poll::Ready(Err(SourceError::Cancelled)),
                        "{label}"
                    );
                    drop(task);
                }
            }

            if !writer_stage {
                assert!(flag.get(), "{label}: pending future was dropped");
            }
            // No retry, drain, second lookup, further BODY or further write.
            assert_eq!(h.io(), io, "{label}");
            if !(writer_stage && matches!(cause, Cause::Drop)) {
                assert!(poisoned(&h.observer), "{label}");
            }
            {
                let state = writer.borrow();
                assert_eq!(state.aborts, 1, "{label}");
                assert_eq!(state.closes, closes, "{label}");
                assert_eq!(state.write_polls, polls, "{label}");
                assert_eq!(state.got.len(), acked, "{label}");
            }
            if acked == 0 {
                assert!(sink.borrow().0.is_empty(), "{label}");
            } else {
                assert_interval(&sink.borrow(), 0, acked as u64);
            }
            h.assert_released();
        }
    }
}
