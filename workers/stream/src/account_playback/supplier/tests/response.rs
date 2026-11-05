//! Generated-fixture tests for the connected account supplier and the shared
//! request authority behind the reviewed response seams. The real
//! `ArticleSupplier` and `RequestControl` clones drive `OriginalAuthorization`,
//! `OriginalPlay` and its pump; only the catalogue, registry, account replies,
//! signals and a tiny writer are fakes. Synthetic bytes, polled by hand: this is
//! not Worker, provider or transport proof.

use super::*;

use nntune_core::{
    nzb::{File, Segment},
    original_source::reader::AsyncArticleSource,
    yenc,
};

use crate::{
    delivered::VecDeliveredSink,
    original_play::{authorize_original, OriginalWriter},
    play_route::PlayRouteRequest,
};

const PART_BYTES: usize = 3;
const FILE_BYTES: u64 = 12;

// ------------------------------------------------------------- fixtures

struct Generated {
    source: OriginalSource,
    bodies: Vec<Vec<u8>>,
}

/// `parts` yEnc articles of `PART_BYTES` bytes each; the file bytes count up
/// from zero. Catalogue sizes are the actual generated article lengths.
fn generated(parts: usize) -> Generated {
    let size = parts * PART_BYTES;
    let bodies: Vec<Vec<u8>> = (0..parts)
        .map(|i| {
            let first = i * PART_BYTES;
            let data: Vec<u8> = (first..first + PART_BYTES).map(|n| n as u8).collect();
            let mut out = format!(
                "=ybegin part={} total={parts} line=128 size={size} name=generated\r\n\
                 =ypart begin={} end={}\r\n",
                i + 1,
                first + 1,
                first + PART_BYTES
            )
            .into_bytes();
            let crc = yenc::encode_body(&mut out, &data).unwrap();
            out.extend(format!("\r\n=yend size={PART_BYTES} pcrc32={crc:08x}\r\n").bytes());
            out
        })
        .collect();
    let file = File {
        ordinal: 0,
        subject: String::new(),
        filename: None,
        role: "content".into(),
        container: "flac".into(),
        posted_at_unix: 0,
        segments: bodies
            .iter()
            .enumerate()
            .map(|(i, body)| Segment {
                ordinal: i,
                number: i + 1,
                message_id: reference(i),
                encoded_bytes: i64::try_from(body.len()).unwrap(),
            })
            .collect(),
    };
    let source = OriginalSource::from_file(TRACK, "job-1", &file, size as u64, None).unwrap();
    Generated { source, bodies }
}

fn requested_index(request: &[u8]) -> usize {
    let value: Value = serde_json::from_slice(request).unwrap();
    let wanted = value["operation"]["ids"].as_str().unwrap();
    (0..).find(|&i| reference(i) == wanted).unwrap()
}

/// Serves the generated article named by each signed BODY request. The
/// `fail_on_body`-th (1-based) BODY is reported unavailable instead.
fn article_reply(bodies: Vec<Vec<u8>>, fail_on_body: Option<usize>) -> ReplyFn {
    let calls = Cell::new(0usize);
    Box::new(move |account, request| {
        calls.set(calls.get() + 1);
        if fail_on_body == Some(calls.get()) {
            return OperationReply::Failed(OperationError::Unavailable);
        }
        OperationReply::Served(Served {
            provider_id: account.to_owned(),
            output: Output::Body(bodies[requested_index(request)].clone()),
        })
    })
}

/// One account, so every BODY and every duplicate is attributable.
fn fixture_rig(parts: usize, fail_on_body: Option<usize>) -> (Rig, Generated) {
    let rig = Rig::with_rows(parts, &[(1, 0)]);
    let fixture = generated(parts);
    *rig.catalogue.mode.borrow_mut() = CatMode::Source(fixture.source.clone());
    *rig.accounts.reply.borrow_mut() = article_reply(fixture.bodies.clone(), fail_on_body);
    (rig, fixture)
}

/// Article indices in BODY dispatch order.
fn requested(rig: &Rig) -> Vec<usize> {
    rig.accounts
        .execute_calls
        .borrow()
        .iter()
        .map(|(_, _, body)| requested_index(body))
        .collect()
}

fn play_request(range: Option<&str>) -> PlayRouteRequest {
    PlayRouteRequest {
        track_id: TRACK.into(),
        token: Some(grant::issue_v2(
            "user", TRACK, "play", 1, GRANT_EXP, GRANT_KEY,
        )),
        range: range.map(str::to_owned),
        trace_id: None,
    }
}

// --------------------------------------------------------------- writer

#[derive(Default)]
struct WriterState {
    got: Vec<u8>,
    /// Pending, without accepting anything, once this many bytes are acked.
    block_after: Option<usize>,
    write_polls: usize,
    closes: usize,
    aborts: usize,
    disconnected: bool,
    waker: Option<Waker>,
}
type WriterRef = Rc<RefCell<WriterState>>;

/// Cancellation registers the current pump waker, including during account
/// I/O and backpressure. Tests drive sink acceptance explicitly.
struct Writer(WriterRef);

impl OriginalWriter for Writer {
    fn poll_cancel(&mut self, cx: &mut Context<'_>) -> Poll<SourceError> {
        let mut state = self.0.borrow_mut();
        state.waker = Some(cx.waker().clone());
        if state.disconnected {
            Poll::Ready(SourceError::Cancelled)
        } else {
            Poll::Pending
        }
    }

    fn poll_write(
        &mut self,
        _cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<Result<usize, SourceError>> {
        let mut state = self.0.borrow_mut();
        state.write_polls += 1;
        if state
            .block_after
            .is_some_and(|limit| state.got.len() >= limit)
        {
            return Poll::Pending;
        }
        state.got.extend_from_slice(bytes);
        Poll::Ready(Ok(bytes.len()))
    }

    fn poll_close(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), SourceError>> {
        self.0.borrow_mut().closes += 1;
        Poll::Ready(Ok(()))
    }

    fn abort(&mut self) {
        self.0.borrow_mut().aborts += 1;
    }
}

// ------------------------------------------------------------- driving

/// Authorizes, prepares a borrowed `OriginalPlay` over a clone of the
/// supplier's source, and pumps it with the supplier itself as the article
/// source and a clone of the supplier's request control as the response
/// control. Nothing is opened until the returned future is polled.
macro_rules! pump_response {
    ($supplier:ident, $response:ident, $source:ident, $sink:ident, $range:expr, $writer:ident) => {{
        let play = authorize_original(&play_request($range), GRANT_KEY, NOW / 1000)
            .unwrap()
            .prepare(&$source, $range, &mut $response, &mut $sink)
            .unwrap();
        Box::pin(play.pump(&mut $supplier, &mut $response, Writer($writer.clone())))
    }};
}

/// Polls until done. A ready supplier may yield once per skipped prefix
/// article, so one poll is not always enough.
fn drive<T>(future: &mut Pin<Box<impl Future<Output = T>>>) -> T {
    for _ in 0..64 {
        if let Poll::Ready(value) = poll(future, Waker::noop()) {
            return value;
        }
    }
    panic!("response did not finish");
}

#[derive(Clone, Copy, Debug)]
enum Revoke {
    Stop,
    Abort,
    RequestTimer,
    Clock,
    OperationTimer,
}

impl Revoke {
    fn fire(self, rig: &Rig, control: &RequestControl<'_>) {
        match self {
            Self::Stop => control.stop(),
            Self::Abort => rig.abort.fire(),
            Self::RequestTimer => rig.req_timer.fire(),
            Self::Clock => rig.now.set(GRANT_EXP_MS),
            Self::OperationTimer => rig.op_timer.fire(),
        }
    }

    /// A clock jump has no waker; the next poll simply observes it.
    fn wakes(self) -> bool {
        !matches!(self, Self::Clock)
    }
}

fn assert_interval(sink: &VecDeliveredSink, start: u64, end: u64) {
    assert_eq!(sink.0.len(), 1);
    let interval = &sink.0[0];
    assert_eq!(
        (interval.start, interval.end, interval.file_bytes),
        (start, end, Some(FILE_BYTES))
    );
    assert_eq!(
        (&*interval.user_id, &*interval.play_id, &*interval.track_id),
        ("user", "play", TRACK)
    );
}

// ---------------------------------------------------------------- tests

#[test]
fn full_and_ranged_responses_stream_generated_bytes_from_the_supplier() {
    // (range, status, expected file bytes, expected BODY order). The prefix
    // walk stops as soon as the range is covered.
    let cases = [
        (None, 200, 0..12u8, vec![3, 0, 1, 2]),
        (Some("bytes=2-8"), 206, 2..9, vec![3, 0, 1, 2]),
        (Some("bytes=0-5"), 206, 0..6, vec![3, 0, 1]),
        (Some("bytes=-3"), 206, 9..12, vec![3, 0, 1, 2]),
    ];
    for (range, status, expected, opens) in cases {
        let (rig, _fixture) = fixture_rig(4, None);
        setup!(rig, clock, timer, control, observer, supplier);
        let mut response = observer.clone();
        let source = supplier.source().clone();
        let length = u64::from(expected.end - expected.start);

        // Metadata comes from the real authorization, before any article I/O.
        let mut meta_sink = VecDeliveredSink::default();
        {
            let meta = authorize_original(&play_request(range), GRANT_KEY, NOW / 1000)
                .unwrap()
                .prepare(&source, range, &mut response, &mut meta_sink)
                .unwrap();
            assert_eq!((meta.status, meta.body_len), (status, length));
            assert!(meta
                .headers
                .contains(&("content-length".to_owned(), length.to_string())));
            assert_eq!(
                meta.headers.contains(&(
                    "content-range".to_owned(),
                    format!("bytes {}-{}/{FILE_BYTES}", expected.start, expected.end - 1)
                )),
                status == 206
            );
        }
        assert!(meta_sink.0.is_empty());
        assert_eq!(rig.io(), (1, 0, 0, 0, 0));

        let mut sink = VecDeliveredSink::default();
        let writer = WriterRef::default();
        let mut future = pump_response!(supplier, response, source, sink, range, writer);
        assert_eq!(rig.body_calls(), 0, "nothing opens before the first poll");
        assert_eq!(drive(&mut future), Ok(()));
        drop(future);

        let state = writer.borrow();
        assert_eq!(state.got, expected.clone().collect::<Vec<u8>>());
        assert_eq!((state.closes, state.aborts), (1, 0));
        assert_eq!(requested(&rig), opens, "{range:?}");
        assert_eq!(supplier.serving_accounts().len(), opens.len());
        assert_interval(&sink, u64::from(expected.start), u64::from(expected.end));
        assert!(live(&observer));
    }
}

#[test]
fn single_account_final_first_reads_each_reference_exactly_once() {
    let (rig, fixture) = fixture_rig(4, None);
    setup!(rig, clock, timer, control, observer, supplier);
    let mut response = observer.clone();
    let source = supplier.source().clone();
    let mut sink = VecDeliveredSink::default();
    let writer = WriterRef::default();
    let mut future = pump_response!(supplier, response, source, sink, None, writer);
    assert_eq!(drive(&mut future), Ok(()));
    drop(future);

    // The final article is proven first and is not fetched again.
    let order = requested(&rig);
    assert_eq!(order, [3, 0, 1, 2]);
    let mut unique = order.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), fixture.bodies.len());
    assert_eq!((rig.registry_calls(), rig.body_calls()), (4, 4));
    assert!(rig
        .accounts
        .execute_calls
        .borrow()
        .iter()
        .all(|(account, _, _)| *account == uid(1)));
    assert_eq!(
        supplier.serving_accounts(),
        [uid(1), uid(1), uid(1), uid(1)]
    );
    assert_eq!(writer.borrow().got, (0..12).collect::<Vec<u8>>());
    assert_interval(&sink, 0, FILE_BYTES);

    // A replay of an already-read article is refused without another BODY.
    let before = rig.io();
    assert_eq!(
        ready(supplier.open(3, &reference(3))).err(),
        Some(SourceError::Binding)
    );
    assert_eq!(rig.io(), before);
    assert!(poisoned(&observer));
}

#[test]
fn shared_revocation_while_body_pending_drops_the_account_future_with_zero_delivery() {
    let causes = [
        Revoke::Stop,
        Revoke::Abort,
        Revoke::RequestTimer,
        Revoke::Clock,
        Revoke::OperationTimer,
    ];
    for cause in causes {
        let (rig, _fixture) = fixture_rig(3, None);
        rig.accounts.execute_pending.set(true);
        setup!(rig, clock, timer, control, observer, supplier);
        let mut response = observer.clone();
        let source = supplier.source().clone();
        let mut sink = VecDeliveredSink::default();
        let writer = WriterRef::default();
        let wakes = Arc::new(Wakes::default());
        let waker = Waker::from(wakes.clone());
        let mut future = pump_response!(supplier, response, source, sink, None, writer);

        assert!(poll(&mut future, &waker).is_pending(), "{cause:?}");
        assert_eq!(requested(&rig), [2], "{cause:?}");
        assert!(!rig.accounts.execute_dropped.get(), "{cause:?}");
        cause.fire(&rig, &observer);
        if cause.wakes() {
            assert!(wakes.0.load(Ordering::Relaxed) > 0, "{cause:?}");
        }
        assert_eq!(
            poll(&mut future, &waker),
            Poll::Ready(Err(SourceError::Cancelled)),
            "{cause:?}"
        );
        drop(future);

        // The pending account future was dropped, never retried or completed.
        assert!(rig.accounts.execute_dropped.get(), "{cause:?}");
        assert_eq!(requested(&rig), [2], "{cause:?}");
        assert!(poisoned(&observer), "{cause:?}");
        assert!(poisoned(&response), "{cause:?}");
        assert!(supplier.serving_accounts().is_empty(), "{cause:?}");
        assert!(sink.0.is_empty(), "{cause:?}");
        let state = writer.borrow();
        assert!(state.got.is_empty(), "{cause:?}");
        assert_eq!(
            (state.write_polls, state.closes, state.aborts),
            (0, 0, 1),
            "{cause:?}"
        );
    }
}

#[test]
fn backpressured_writer_blocks_the_next_body_until_it_accepts_again() {
    let (rig, _fixture) = fixture_rig(4, None);
    setup!(rig, clock, timer, control, observer, supplier);
    let mut response = observer.clone();
    let source = supplier.source().clone();
    let mut sink = VecDeliveredSink::default();
    let writer = WriterRef::default();
    writer.borrow_mut().block_after = Some(PART_BYTES);
    let mut future = pump_response!(supplier, response, source, sink, None, writer);

    for _ in 0..3 {
        assert!(poll(&mut future, Waker::noop()).is_pending());
    }
    // Slice 0 was accepted and slice 1 is blocked; article 2 is never opened.
    assert_eq!(requested(&rig), [3, 0, 1]);
    assert_eq!(writer.borrow().got, [0, 1, 2]);

    writer.borrow_mut().block_after = None;
    assert_eq!(drive(&mut future), Ok(()));
    drop(future);
    assert_eq!(requested(&rig), [3, 0, 1, 2]);
    let state = writer.borrow();
    assert_eq!(state.got, (0..12).collect::<Vec<u8>>());
    assert_eq!((state.closes, state.aborts), (1, 0));
    assert_interval(&sink, 0, FILE_BYTES);
}

#[test]
fn revocation_wakes_a_backpressured_writer_and_reports_only_the_acknowledged_prefix() {
    let causes = [
        Revoke::Stop,
        Revoke::Abort,
        Revoke::RequestTimer,
        Revoke::Clock,
    ];
    for cause in causes {
        let (rig, _fixture) = fixture_rig(4, None);
        setup!(rig, clock, timer, control, observer, supplier);
        let mut response = observer.clone();
        let source = supplier.source().clone();
        let mut sink = VecDeliveredSink::default();
        let writer = WriterRef::default();
        writer.borrow_mut().block_after = Some(PART_BYTES);
        let wakes = Arc::new(Wakes::default());
        let waker = Waker::from(wakes.clone());
        let mut future = pump_response!(supplier, response, source, sink, None, writer);

        assert!(poll(&mut future, &waker).is_pending(), "{cause:?}");
        assert_eq!(requested(&rig), [3, 0, 1], "{cause:?}");
        let polls = writer.borrow().write_polls;
        cause.fire(&rig, &observer);
        if cause.wakes() {
            assert!(wakes.0.load(Ordering::Relaxed) > 0, "{cause:?}");
        }
        assert_eq!(
            poll(&mut future, &waker),
            Poll::Ready(Err(SourceError::Cancelled)),
            "{cause:?}"
        );
        drop(future);

        // Revoked before the writer was polled again; no further BODY.
        assert_eq!(requested(&rig), [3, 0, 1], "{cause:?}");
        assert!(poisoned(&observer), "{cause:?}");
        assert!(poisoned(&response), "{cause:?}");
        let state = writer.borrow();
        assert_eq!(state.got, [0, 1, 2], "{cause:?}");
        assert_eq!(
            (state.write_polls, state.closes, state.aborts),
            (polls, 0, 1),
            "{cause:?}"
        );
        assert_interval(&sink, 0, PART_BYTES as u64);
    }
}

#[test]
fn supplier_failure_poisons_the_response_clone_and_keeps_the_acknowledged_prefix() {
    // (failing BODY number, acknowledged bytes before it)
    for (fail_on, acked) in [(1usize, 0usize), (3, 3)] {
        let (rig, _fixture) = fixture_rig(4, Some(fail_on));
        setup!(rig, clock, timer, control, observer, supplier);
        let mut response = observer.clone();
        let source = supplier.source().clone();
        let mut sink = VecDeliveredSink::default();
        let writer = WriterRef::default();
        let mut future = pump_response!(supplier, response, source, sink, None, writer);
        assert_eq!(
            drive(&mut future),
            Err(SourceError::Transport),
            "fail on {fail_on}"
        );
        drop(future);

        assert!(poisoned(&observer), "fail on {fail_on}");
        assert!(poisoned(&response), "fail on {fail_on}");
        assert_eq!(rig.body_calls(), fail_on, "no retry or failover");
        assert_eq!(supplier.serving_accounts().len(), fail_on - 1);
        {
            let state = writer.borrow();
            assert_eq!(state.got.len(), acked, "fail on {fail_on}");
            assert_eq!((state.closes, state.aborts), (0, 1), "fail on {fail_on}");
        }
        if acked == 0 {
            assert!(sink.0.is_empty());
        } else {
            assert_interval(&sink, 0, acked as u64);
        }

        // The dead shared authority refuses any further open without I/O.
        let before = rig.io();
        assert_eq!(
            ready(supplier.open(2, &reference(2))).err(),
            Some(SourceError::Cancelled)
        );
        assert_eq!(rig.io(), before);
    }
}

#[test]
fn writer_disconnect_drops_a_pending_supplier_body_without_delivery_or_retry() {
    let (rig, _fixture) = fixture_rig(4, None);
    rig.accounts.execute_pending.set(true);
    setup!(rig, clock, timer, control, observer, supplier);
    let mut response = observer.clone();
    let source = supplier.source().clone();
    let mut sink = VecDeliveredSink::default();
    let writer = WriterRef::default();
    let wakes = Arc::new(Wakes::default());
    let waker = Waker::from(wakes.clone());
    let mut future = pump_response!(supplier, response, source, sink, None, writer);
    assert!(poll(&mut future, &waker).is_pending());
    assert_eq!(requested(&rig), [3]);

    let registered = {
        let mut state = writer.borrow_mut();
        state.disconnected = true;
        state.waker.take().unwrap()
    };
    registered.wake();
    assert!(wakes.0.load(Ordering::Relaxed) > 0);
    assert_eq!(
        poll(&mut future, &waker),
        Poll::Ready(Err(SourceError::Cancelled))
    );
    drop(future);

    assert!(rig.accounts.execute_dropped.get());
    assert!(poisoned(&observer));
    assert!(poisoned(&response));
    assert_eq!(requested(&rig), [3]);
    assert!(sink.0.is_empty());
    assert!(supplier.serving_accounts().is_empty());
    let state = writer.borrow();
    assert!(state.got.is_empty());
    assert_eq!((state.write_polls, state.closes, state.aborts), (0, 0, 1));
}

#[test]
fn trait_open_returns_the_generated_article_once_in_requested_order() {
    let (rig, fixture) = fixture_rig(4, None);
    setup!(rig, clock, timer, control, observer, supplier);
    for index in [3usize, 0] {
        let name = reference(index);
        let bytes = ready(AsyncArticleSource::open(&mut supplier, index, &name)).unwrap();
        assert_eq!(bytes, fixture.bodies[index]);
    }
    assert_eq!(requested(&rig), [3, 0]);
    assert_eq!(supplier.serving_accounts(), [uid(1), uid(1)]);
    assert!(live(&observer));

    let before = rig.io();
    assert_eq!(
        ready(AsyncArticleSource::open(&mut supplier, 3, &reference(3))).err(),
        Some(SourceError::Binding)
    );
    assert_eq!(rig.io(), before, "duplicate dispatches no BODY");
    assert!(poisoned(&observer));
}

#[test]
fn trait_open_binds_index_and_reference_before_any_io() {
    let cases = [
        (9usize, reference(9), SourceError::Bounds),
        (4, reference(4), SourceError::Bounds),
        (0, reference(1), SourceError::Binding),
        (3, reference(0), SourceError::Binding),
        (0, String::new(), SourceError::Binding),
    ];
    for (index, wrong, expected) in cases {
        let (rig, _fixture) = fixture_rig(4, None);
        setup!(rig, clock, timer, control, observer, supplier);
        let response = observer.clone();
        let before = rig.io();
        assert_eq!(before, (1, 0, 0, 0, 0));
        assert_eq!(
            ready(AsyncArticleSource::open(&mut supplier, index, &wrong)).err(),
            Some(expected),
            "index {index} ref {wrong:?}"
        );
        assert_eq!(rig.io(), before, "index {index} ref {wrong:?}");
        assert!(poisoned(&observer));
        assert!(poisoned(&response));
        assert!(supplier.serving_accounts().is_empty());
    }
}
