//! Synthetic byte patterns only. Off-route tests, not Worker/provider proof.
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    future::Future,
    rc::Rc,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    task::{Context, Poll, Wake, Waker},
};

use nntune_core::{
    nzb::{File, Segment},
    original_source::{
        reader::{AsyncArticleSource, AsyncReadControl, ReadControl},
        OriginalSource, SourceError,
    },
    yenc,
};
use nntune_stream::{
    delivered::VecDeliveredSink,
    grant,
    original_play::{
        authorize_original, control::OriginalRequestControl, OriginalControl, OriginalWriter,
    },
    play_route::PlayRouteRequest,
};

const TRACK: &str = "00000000-0000-0000-0000-000000000001";
const SECRET: &[u8] = b"generated-test-signing-material";
const NOW: u64 = 1000;

fn request(range: Option<&str>) -> PlayRouteRequest {
    PlayRouteRequest {
        track_id: TRACK.into(),
        token: Some(grant::issue_v2(
            "user_generated",
            TRACK,
            "play_generated",
            1,
            NOW + 60,
            SECRET,
        )),
        range: range.map(str::to_owned),
        trace_id: None,
    }
}

#[derive(Default)]
struct ControlState {
    cancelled: bool,
    waker: Option<Waker>,
}

#[derive(Clone)]
struct Control {
    now: Rc<Cell<u64>>,
    state: Rc<RefCell<ControlState>>,
}
impl Control {
    fn new() -> Self {
        Self {
            now: Rc::new(Cell::new(NOW)),
            state: Rc::default(),
        }
    }
}
impl ReadControl for Control {
    fn check(&mut self) -> Result<(), SourceError> {
        if self.state.borrow().cancelled {
            Err(SourceError::Cancelled)
        } else {
            Ok(())
        }
    }
}
impl AsyncReadControl for Control {
    fn poll_cancel(&mut self, cx: &mut Context<'_>) -> Poll<SourceError> {
        let mut state = self.state.borrow_mut();
        if state.cancelled {
            Poll::Ready(SourceError::Cancelled)
        } else {
            state.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}
impl OriginalControl for Control {
    fn now_unix(&self) -> u64 {
        self.now.get()
    }
}

struct Articles {
    bodies: Vec<Vec<u8>>,
    opened: Vec<usize>,
    fail: Option<usize>,
    pending: Option<usize>,
    expire: Option<Rc<Cell<u64>>>,
    log: Rc<RefCell<Vec<usize>>>, // readable while a pump borrows the source
}
impl AsyncArticleSource for Articles {
    async fn open(&mut self, index: usize, reference: &str) -> Result<Vec<u8>, SourceError> {
        assert_eq!(reference, format!("generated-{index}@fixture.invalid"));
        self.opened.push(index);
        self.log.borrow_mut().push(index);
        if self.pending == Some(index) {
            std::future::pending::<()>().await;
        }
        if let Some(now) = &self.expire {
            now.set(NOW + 60);
        }
        if self.fail == Some(index) {
            return Err(SourceError::Transport);
        }
        Ok(self.bodies[index].clone())
    }
}

fn fixture() -> (OriginalSource, Articles) {
    let file = File {
        ordinal: 0,
        subject: String::new(),
        filename: None,
        role: "content".into(),
        container: "flac".into(),
        posted_at_unix: 0,
        segments: (0..3)
            .map(|i| Segment {
                ordinal: i,
                number: i + 1,
                message_id: format!("generated-{i}@fixture.invalid"),
                encoded_bytes: 512,
            })
            .collect(),
    };
    let source = OriginalSource::from_file(TRACK, "job_generated", &file, 9, None).unwrap();
    let bodies = (0..3).map(|i| {
        let mut out = format!("=ybegin part={} total=3 line=128 size=9 name=generated\r\n=ypart begin={} end={}\r\n", i+1, i*3+1, i*3+3).into_bytes();
        let data = [i as u8*3, i as u8*3+1, i as u8*3+2];
        let crc = yenc::encode_body(&mut out, &data).unwrap();
        out.extend(format!("\r\n=yend size=3 pcrc32={crc:08x}\r\n").bytes());
        out
    }).collect();
    (
        source,
        Articles {
            bodies,
            opened: Vec::new(),
            fail: None,
            pending: None,
            expire: None,
            log: Rc::default(),
        },
    )
}

#[tokio::test]
async fn full_and_seek_responses_have_exact_metadata_bytes_and_intervals() {
    for (header, status, expected) in [
        (None, 200, 0..9),
        (Some("bytes=2-6"), 206, 2..7),
        (Some("bytes=-2"), 206, 7..9),
        (Some("bytes=4-"), 206, 4..9),
        (Some("bytes=2-999"), 206, 2..9),
        (Some("bytes=1-2,4-5"), 200, 0..9),
    ] {
        let (source, mut articles) = fixture();
        let mut sink = VecDeliveredSink::default();
        let mut control = Control::new();
        let req = request(header);
        let authority = authorize_original(&req, SECRET, NOW).unwrap();
        let mut play = authority
            .prepare(&source, req.range.as_deref(), &mut control, &mut sink)
            .unwrap();
        assert_eq!(play.status, status);
        assert_eq!(play.body_len, expected.end - expected.start);
        assert_eq!(
            play.headers
                .iter()
                .find(|h| h.0 == "content-length")
                .unwrap()
                .1,
            play.body_len.to_string()
        );
        assert_eq!(play.headers.len(), if status == 206 { 4 } else { 3 });
        if status == 206 {
            assert!(play.headers.contains(&(
                "content-range".into(),
                format!("bytes {}-{}/9", expected.start, expected.end - 1)
            )));
        }
        let mut output = Vec::new();
        while let Some(bytes) = play.next(&mut articles, &mut control).await.unwrap() {
            assert_eq!(
                play.served_bytes(),
                output.len() as u64,
                "pull is not delivery"
            );
            play.written(bytes.len()).unwrap();
            output.extend(bytes);
        }
        assert_eq!(
            output,
            expected.clone().map(|n| n as u8).collect::<Vec<_>>()
        );
        play.finish().unwrap();
        assert_eq!(sink.0.len(), 1);
        let interval = &sink.0[0];
        assert_eq!(
            (interval.start, interval.end, interval.file_bytes),
            (expected.start, expected.end, Some(9))
        );
        assert_eq!(
            (&*interval.user_id, &*interval.play_id, &*interval.track_id),
            ("user_generated", "play_generated", TRACK)
        );
        assert_eq!(articles.opened.first(), Some(&2));
        assert!(articles.opened.len() <= 3);
        // Existing meter DTO remains independent of source/account ownership.
        let encoded = serde_json::to_vec(interval).unwrap();
        assert_eq!(
            serde_json::from_slice::<nntune_stream::delivered::DeliveredInterval>(&encoded)
                .unwrap(),
            *interval
        );
    }
}

#[test]
fn invalid_grants_precede_source_resolution_and_emit_nothing() {
    let mut unconfigured = request(None);
    unconfigured.token = Some(grant::issue_v2(
        "user_generated",
        TRACK,
        "play_generated",
        1,
        NOW + 60,
        b"",
    ));
    assert_eq!(
        authorize_original(&unconfigured, b"", NOW)
            .err()
            .unwrap()
            .status,
        500
    );
    let mut bads = vec![
        None,
        Some("forged".into()),
        Some(grant::issue("user_generated", TRACK, NOW + 60, SECRET)),
        Some(grant::issue_v2(
            "user_generated",
            TRACK,
            "play_generated",
            1,
            NOW,
            SECRET,
        )),
        Some(grant::issue_v2(
            "user_generated",
            TRACK,
            "play_generated",
            1,
            NOW + grant::MAX_GRANT_TTL + grant::CLOCK_SKEW_SECS + 1,
            SECRET,
        )),
    ];
    let mut forged = request(None).token.unwrap();
    forged.push('x');
    bads.push(Some(forged));
    for token in bads {
        let mut req = request(None);
        req.token = token;
        // Lookup is deliberately after this capability gate, never in it.
        let result = authorize_original(&req, SECRET, NOW);
        assert_eq!(result.err().unwrap().status, 401);
    }
}

#[test]
fn wrong_track_grants_distinguish_verified_v2_from_unauthorized_tokens() {
    let (source, articles) = fixture();
    let mut control = Control::new();
    let mut sink = VecDeliveredSink::default();
    for (token, status, code) in [
        (
            grant::issue_v2(
                "user_generated",
                "other",
                "play_generated",
                1,
                NOW + 60,
                SECRET,
            ),
            403,
            "forbidden",
        ),
        (
            grant::issue("user_generated", "other", NOW + 60, SECRET),
            401,
            "unauthorized",
        ),
        (
            grant::issue_v2("user_generated", "other", "play_generated", 1, NOW, SECRET),
            401,
            "unauthorized",
        ),
        (
            grant::issue_v2(
                "user_generated",
                "other",
                "play_generated",
                1,
                NOW + 60,
                b"wrong-signing-material",
            ),
            401,
            "unauthorized",
        ),
    ] {
        let mut req = request(None);
        req.token = Some(token);
        // A refusal must never produce the capability needed for source access.
        let err = match authorize_original(&req, SECRET, NOW) {
            Ok(authority) => {
                let _ = authority.prepare(&source, None, &mut control, &mut sink);
                panic!("wrong-track grant authorized");
            }
            Err(err) => err,
        };
        assert_eq!(err.status, status);
        assert_eq!(err.api_error.code, code);
        assert_eq!(err.content_range, None);
    }
    assert!(articles.opened.is_empty() && sink.0.is_empty());
}

#[tokio::test]
async fn partial_acknowledgement_and_drop_report_once_without_drain() {
    for explicit_abort in [false, true] {
        let (source, mut articles) = fixture();
        let mut sink = VecDeliveredSink::default();
        let mut control = Control::new();
        let mut play = authorize_original(&request(Some("bytes=1-7")), SECRET, NOW)
            .unwrap()
            .prepare(&source, Some("bytes=1-7"), &mut control, &mut sink)
            .unwrap();
        let bytes = play
            .next(&mut articles, &mut control)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(bytes, vec![1, 2]);
        play.written(1).unwrap();
        if explicit_abort {
            play.abort();
        } else {
            drop(play);
        }
        assert_eq!(articles.opened, vec![2, 0]);
        assert_eq!(sink.0.len(), 1);
        assert_eq!((sink.0[0].start, sink.0[0].end), (1, 2));
    }
}

#[tokio::test]
async fn zero_acknowledgement_incomplete_finish_and_errors_never_fabricate_delivery() {
    for mode in 0..4 {
        let (source, mut articles) = fixture();
        let mut sink = VecDeliveredSink::default();
        let mut control = Control::new();
        let mut play = authorize_original(&request(None), SECRET, NOW)
            .unwrap()
            .prepare(&source, None, &mut control, &mut sink)
            .unwrap();
        match mode {
            0 => {
                play.abort();
            }
            1 => {
                play.next(&mut articles, &mut control).await.unwrap();
                drop(play);
            }
            2 => {
                play.next(&mut articles, &mut control).await.unwrap();
                assert_eq!(play.written(4), Err(SourceError::Bounds));
                drop(play);
            }
            _ => {
                assert_eq!(play.finish(), Err(SourceError::Transport));
            }
        }
        assert!(sink.0.is_empty());
    }
}

#[tokio::test]
async fn provider_failure_after_output_reports_only_acknowledged_bytes() {
    let (source, mut articles) = fixture();
    articles.fail = Some(1);
    let mut sink = VecDeliveredSink::default();
    let mut control = Control::new();
    let mut play = authorize_original(&request(None), SECRET, NOW)
        .unwrap()
        .prepare(&source, None, &mut control, &mut sink)
        .unwrap();
    let bytes = play
        .next(&mut articles, &mut control)
        .await
        .unwrap()
        .unwrap();
    play.written(bytes.len()).unwrap();
    assert_eq!(
        play.next(&mut articles, &mut control).await,
        Err(SourceError::Transport)
    );
    assert_eq!(
        play.next(&mut articles, &mut control).await,
        Err(SourceError::Cancelled)
    );
    assert_eq!(play.finish(), Err(SourceError::Transport));
    assert_eq!(articles.opened, vec![2, 0, 1]);
    assert_eq!(sink.0.len(), 1);
    assert_eq!((sink.0[0].start, sink.0[0].end), (0, 3));
}

#[tokio::test]
async fn expiry_after_source_or_article_await_releases_no_media() {
    let (source, mut articles) = fixture();
    let mut sink = VecDeliveredSink::default();
    let mut control = Control::new();
    let authority = authorize_original(&request(None), SECRET, NOW).unwrap();
    control.now.set(NOW + 60); // Source resolver completed after expiry.
    assert_eq!(
        authority
            .prepare(&source, None, &mut control, &mut sink)
            .err()
            .unwrap()
            .status,
        401
    );
    assert!(articles.opened.is_empty() && sink.0.is_empty());
    control.now.set(NOW);
    articles.expire = Some(control.now.clone());
    let mut play = authorize_original(&request(None), SECRET, NOW)
        .unwrap()
        .prepare(&source, None, &mut control, &mut sink)
        .unwrap();
    assert_eq!(
        play.next(&mut articles, &mut control).await,
        Err(SourceError::Cancelled)
    );
    drop(play);
    assert_eq!(articles.opened, vec![2]);
    assert!(sink.0.is_empty());
}

#[test]
fn unsatisfiable_range_and_wrong_source_binding_do_no_article_io() {
    let (source, articles) = fixture();
    let mut control = Control::new();
    let mut sink = VecDeliveredSink::default();
    for range in ["bytes=9-", "bytes=-0"] {
        let authority = authorize_original(&request(Some(range)), SECRET, NOW).unwrap();
        let err = authority
            .prepare(&source, Some(range), &mut control, &mut sink)
            .err()
            .unwrap();
        assert_eq!(err.status, 416);
        assert_eq!(err.api_error.code, "range_not_satisfiable");
        assert_eq!(err.content_range.as_deref(), Some("bytes */9"));
        assert_eq!(err.retry_after, None);
    }
    let mut req = request(None);
    req.track_id = "other".into();
    req.token = Some(grant::issue_v2(
        "user_generated",
        "other",
        "play_generated",
        1,
        NOW + 60,
        SECRET,
    ));
    let authority = authorize_original(&req, SECRET, NOW).unwrap();
    assert_eq!(
        authority
            .prepare(&source, None, &mut control, &mut sink)
            .err()
            .unwrap()
            .status,
        502
    );
    assert!(articles.opened.is_empty() && sink.0.is_empty());
}

#[tokio::test]
async fn dropping_a_pending_pull_after_output_settles_partial_delivery_without_retry() {
    let (source, mut articles) = fixture();
    let mut sink = VecDeliveredSink::default();
    let mut control = Control::new();
    let mut play = authorize_original(&request(None), SECRET, NOW)
        .unwrap()
        .prepare(&source, None, &mut control, &mut sink)
        .unwrap();
    let bytes = play
        .next(&mut articles, &mut control)
        .await
        .unwrap()
        .unwrap();
    play.written(bytes.len()).unwrap();
    articles.pending = Some(1);
    let mut cx = Context::from_waker(Waker::noop());
    {
        let mut next = Box::pin(play.next(&mut articles, &mut control));
        assert!(next.as_mut().poll(&mut cx).is_pending());
    }
    assert_eq!(
        play.next(&mut articles, &mut control).await,
        Err(SourceError::Cancelled)
    );
    drop(play);
    assert_eq!(articles.opened, vec![2, 0, 1]);
    assert_eq!(sink.0.len(), 1);
    assert_eq!((sink.0[0].start, sink.0[0].end), (0, 3));
}

// ---- Fake writer pump regressions: manual polling, no timers or runtime. ----

enum Step {
    Accept(usize),
    Zero,
    Over,
    Fail,
    Block, // from here on poll_write stays Pending
}

#[derive(Default)]
struct WState {
    script: VecDeque<Step>, // empty script accepts everything
    block: bool,            // poll_write stays Pending, storing the waker
    cancel: Option<SourceError>,
    close: Option<SourceError>,
    close_block: bool,
    got: Vec<u8>,
    write_polls: usize,
    closes: usize,
    aborts: usize,
    at_close: usize,
    waker: Option<Waker>,
    close_waker: Option<Waker>,
}
type Shared = Rc<RefCell<WState>>;
struct Writer(Shared);

impl OriginalWriter for Writer {
    fn poll_cancel(&mut self, cx: &mut Context<'_>) -> Poll<SourceError> {
        let mut st = self.0.borrow_mut();
        match st.cancel.take() {
            Some(e) => Poll::Ready(e),
            None => {
                st.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
    fn poll_write(
        &mut self,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<Result<usize, SourceError>> {
        let mut st = self.0.borrow_mut();
        st.write_polls += 1;
        if let Some(Step::Block) = st.script.front() {
            st.script.pop_front();
            st.block = true;
        }
        if st.block {
            st.waker = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let n = match st.script.pop_front() {
            None => bytes.len(),
            Some(Step::Accept(n)) => n.min(bytes.len()),
            Some(Step::Zero) => 0,
            Some(Step::Over) => return Poll::Ready(Ok(bytes.len() + 1)),
            Some(Step::Fail) => return Poll::Ready(Err(SourceError::Transport)),
            Some(Step::Block) => unreachable!(),
        };
        st.got.extend_from_slice(&bytes[..n.min(bytes.len())]);
        Poll::Ready(Ok(n))
    }
    fn poll_close(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), SourceError>> {
        let mut st = self.0.borrow_mut();
        st.closes += 1;
        st.at_close = st.got.len();
        if st.close_block {
            st.close_waker = Some(cx.waker().clone());
            return Poll::Pending;
        }
        Poll::Ready(st.close.take().map_or(Ok(()), Err))
    }
    fn abort(&mut self) {
        self.0.borrow_mut().aborts += 1;
    }
}

fn shared(steps: impl IntoIterator<Item = Step>) -> Shared {
    Rc::new(RefCell::new(WState {
        script: steps.into_iter().collect(),
        ..WState::default()
    }))
}

#[derive(Clone, Copy)]
enum ControlEvent {
    Cancel,
    Expire,
}
impl ControlEvent {
    fn fire(self, now: &Cell<u64>, state: &RefCell<ControlState>) {
        let waker = {
            let mut state = state.borrow_mut();
            match self {
                Self::Cancel => state.cancelled = true,
                Self::Expire => now.set(NOW + 60),
            }
            state.waker.take().expect("control registered a waker")
        };
        // Only the control's registered waker schedules these cancellation polls.
        waker.wake();
    }
}

#[derive(Default)]
struct Wakes(AtomicUsize);
impl Wake for Wakes {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

macro_rules! pump {
    ($source:expr, $range:expr, $articles:expr, $control:expr, $sink:expr, $state:expr) => {{
        let play = authorize_original(&request($range), SECRET, NOW)
            .unwrap()
            .prepare(&$source, $range, &mut $control, &mut $sink)
            .unwrap();
        Box::pin(play.pump(&mut $articles, &mut $control, Writer($state.clone())))
    }};
}

#[test]
fn owned_plan_shares_metadata_refusals_and_opens_nothing_before_pumping() {
    for range in [None, Some("bytes=2-6"), Some("bytes=-2"), Some("bytes=4-")] {
        let (source, articles) = fixture();
        let mut control = Control::new();
        let mut sink = VecDeliveredSink::default();
        let borrowed = authorize_original(&request(range), SECRET, NOW)
            .unwrap()
            .prepare(&source, range, &mut control, &mut sink)
            .unwrap();
        let owned = authorize_original(&request(range), SECRET, NOW)
            .unwrap()
            .prepare_owned(source.clone(), range, &mut control)
            .unwrap();
        assert_eq!(
            (owned.status(), owned.body_len()),
            (borrowed.status, borrowed.body_len)
        );
        assert_eq!(owned.headers(), borrowed.headers);
        drop((borrowed, owned));
        assert!(articles.opened.is_empty());
        assert!(sink.0.is_empty());
    }
    for mode in 0..4 {
        let (source, articles) = fixture();
        let mut control = Control::new();
        let mut req = request(None);
        let range = match mode {
            0 => Some("bytes=9-"),
            3 => Some("bytes=-0"),
            _ => None,
        };
        if mode == 1 {
            req.track_id = "00000000-0000-0000-0000-000000000002".into();
            req.token = Some(grant::issue_v2(
                "user_generated",
                &req.track_id,
                "play_generated",
                1,
                NOW + 60,
                SECRET,
            ));
        }
        let authority = authorize_original(&req, SECRET, NOW).unwrap();
        if mode == 2 {
            control.now.set(NOW + 60);
        }
        let err = authority
            .prepare_owned(source, range, &mut control)
            .err()
            .expect("refusal");
        assert_eq!(err.status, [416, 502, 401, 416][mode]);
        assert_eq!(err.content_range.as_deref(), range.map(|_| "bytes */9"));
        assert!(articles.opened.is_empty());
    }
}

#[test]
fn owned_unpolled_pump_aborts_writer_without_io_or_delivery() {
    let (source, mut articles) = fixture();
    let (mut sink, mut control) = (VecDeliveredSink::default(), Control::new());
    let st = shared([]);
    let owned = authorize_original(&request(None), SECRET, NOW)
        .unwrap()
        .prepare_owned(source, None, &mut control)
        .unwrap();
    let future = owned.pump(&mut articles, &mut control, &mut sink, Writer(st.clone()));
    drop(future);
    assert!(articles.opened.is_empty());
    assert!(sink.0.is_empty());
    let st = st.borrow();
    assert_eq!((st.aborts, st.closes, st.write_polls), (1, 0, 0));
}

struct ReportSink(Rc<RefCell<Vec<nntune_stream::delivered::DeliveredInterval>>>);
impl nntune_stream::delivered::DeliveredSink for ReportSink {
    fn delivered(&mut self, interval: nntune_stream::delivered::DeliveredInterval) {
        self.0.borrow_mut().push(interval);
    }
}

#[test]
fn owned_static_task_settles_success_error_and_drop_prefixes_once() {
    // A real request-lifetime task can own all inputs, with the reporting sink
    // constructed inside it. No source borrow or live handler stack is needed.
    for mode in 0..6 {
        let (source, mut articles) = fixture();
        let log = articles.log.clone();
        let mut control = Control::new();
        let owned = authorize_original(&request(Some("bytes=1-7")), SECRET, NOW)
            .unwrap()
            .prepare_owned(source, Some("bytes=1-7"), &mut control)
            .unwrap();
        let st = shared([]);
        match mode {
            1 => articles.pending = Some(2),
            2 => st.borrow_mut().script = [Step::Accept(1), Step::Block].into(),
            3 => st.borrow_mut().close_block = true,
            4 => articles.fail = Some(1),
            5 => st.borrow_mut().script = [Step::Accept(1), Step::Fail].into(),
            _ => {}
        }
        let reports = Rc::new(RefCell::new(Vec::new()));
        let (writer, out) = (Writer(st.clone()), reports.clone());
        let task = async move {
            let mut sink = ReportSink(out);
            owned
                .pump(&mut articles, &mut control, &mut sink, writer)
                .await
        };
        fn require_static(_: &(impl Future<Output = Result<(), SourceError>> + 'static)) {}
        require_static(&task);
        let mut future = Box::pin(task);
        let result = future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()));
        match mode {
            0 => assert_eq!(result, Poll::Ready(Ok(()))),
            4 | 5 => assert_eq!(result, Poll::Ready(Err(SourceError::Transport))),
            _ => assert!(result.is_pending()),
        }
        drop(future);
        let st = st.borrow();
        let expected = [7, 0, 1, 7, 2, 1][mode];
        assert_eq!(st.got, (1..1 + expected as u8).collect::<Vec<_>>());
        assert_eq!(
            (st.closes, st.aborts),
            ([1, 0, 0, 1, 0, 0][mode], usize::from(mode != 0))
        );
        let reports = reports.borrow();
        assert_eq!(reports.len(), usize::from(expected != 0));
        if expected != 0 {
            assert_eq!(
                (reports[0].start, reports[0].end, reports[0].file_bytes),
                (1, 1 + expected, Some(9))
            );
            assert_eq!(
                (
                    &*reports[0].user_id,
                    &*reports[0].play_id,
                    &*reports[0].track_id
                ),
                ("user_generated", "play_generated", TRACK)
            );
        }
        assert_eq!(log.borrow().len(), [3, 1, 2, 3, 3, 2][mode]);
    }
}

#[test]
fn shared_source_plan_retains_the_same_allocation_and_releases_refusals() {
    for range in [None, Some("bytes=2-6"), Some("bytes=9-"), Some("bytes=-0")] {
        let (source, articles) = fixture();
        let source = Rc::new(source);
        let mut control = Control::new();
        let mut sink = VecDeliveredSink::default();
        let borrowed = authorize_original(&request(range), SECRET, NOW)
            .unwrap()
            .prepare(&source, range, &mut control, &mut sink);
        let owned = authorize_original(&request(range), SECRET, NOW)
            .unwrap()
            .prepare_owned(source.clone(), range, &mut control);
        match (borrowed, owned) {
            (Ok(borrowed), Ok(owned)) => {
                assert_eq!(Rc::strong_count(&source), 2);
                assert_eq!(
                    (owned.status(), owned.body_len()),
                    (borrowed.status, borrowed.body_len)
                );
                assert_eq!(owned.headers(), borrowed.headers);
                drop(owned);
            }
            (Err(borrowed), Err(owned)) => {
                assert_eq!(
                    (owned.status, owned.content_range),
                    (borrowed.status, borrowed.content_range)
                );
            }
            _ => panic!("shared and borrowed plans disagree"),
        }
        assert_eq!(Rc::strong_count(&source), 1);
        assert!(articles.opened.is_empty());
        assert!(sink.0.is_empty());
    }
}

// Synthetic retained-input transport only; not the production account supplier.
struct RetainedArticles {
    source: Rc<OriginalSource>,
    control: Control,
    inner: Articles,
}
impl AsyncArticleSource for RetainedArticles {
    async fn open(&mut self, index: usize, reference: &str) -> Result<Vec<u8>, SourceError> {
        self.control.check()?;
        assert_eq!(self.source.article(index)?.reference(), reference);
        self.inner.open(index, reference).await
    }
}

#[test]
fn shared_source_owned_inputs_release_on_unpolled_and_pending_task_drop_or_cancel() {
    async fn scheduled<F: Future<Output = Result<(), SourceError>>>(
        pump: F,
    ) -> Result<(), SourceError> {
        pump.await
    }
    for cancel in [false, true] {
        // Unpolled, pending article, pending write, pending close, success.
        for phase in 0..5 {
            let (source, mut articles) = fixture();
            let source = Rc::new(source);
            let weak = Rc::downgrade(&source);
            let log = articles.log.clone();
            let mut control = Control::new();
            let observed_control = control.clone();
            let owned = authorize_original(&request(Some("bytes=1-7")), SECRET, NOW)
                .unwrap()
                .prepare_owned(source.clone(), Some("bytes=1-7"), &mut control)
                .unwrap();
            let st = shared([]);
            match phase {
                1 => articles.pending = Some(2),
                2 => st.borrow_mut().script = [Step::Accept(1), Step::Block].into(),
                3 => st.borrow_mut().close_block = true,
                _ => {}
            }
            let transport = RetainedArticles {
                source: source.clone(),
                control: control.clone(),
                inner: articles,
            };
            assert_eq!(Rc::strong_count(&source), 3);
            drop(source);
            let reports = Rc::new(RefCell::new(Vec::new()));
            // Own the guard and every input before the scheduler wraps the task.
            let task = scheduled(owned.pump_owned(
                transport,
                control,
                ReportSink(reports.clone()),
                Writer(st.clone()),
            ));
            fn require_static(_: &(impl Future<Output = Result<(), SourceError>> + 'static)) {}
            require_static(&task);
            let mut future = Box::pin(task);
            let wake = Arc::new(Wakes::default());
            let waker = Waker::from(wake.clone());
            let mut cx = Context::from_waker(&waker);
            if phase != 0 {
                let result = future.as_mut().poll(&mut cx);
                if phase == 4 {
                    assert_eq!(result, Poll::Ready(Ok(())));
                } else {
                    assert!(result.is_pending());
                    assert_eq!(weak.strong_count(), 2);
                    if cancel {
                        ControlEvent::Cancel.fire(&observed_control.now, &observed_control.state);
                        assert!(wake.0.load(Ordering::SeqCst) > 0);
                        assert_eq!(
                            future.as_mut().poll(&mut cx),
                            Poll::Ready(Err(SourceError::Cancelled))
                        );
                    }
                }
            }
            drop(future);
            assert!(weak.upgrade().is_none());
            assert_eq!(Rc::strong_count(&observed_control.state), 1);
            assert_eq!(Rc::strong_count(&observed_control.now), 1);
            let st = st.borrow();
            let expected = [0, 0, 1, 7, 7][phase];
            assert_eq!(st.got, (1..1 + expected as u8).collect::<Vec<_>>());
            assert_eq!(st.aborts, usize::from(phase != 4));
            assert_eq!(st.closes, usize::from(phase >= 3));
            assert_eq!(st.write_polls, [0, 0, 2, 3, 3][phase]);
            assert_eq!(log.borrow().len(), [0, 1, 2, 3, 3][phase]);
            let reports = reports.borrow();
            assert_eq!(reports.len(), usize::from(expected != 0));
            if expected != 0 {
                assert_eq!(
                    (reports[0].start, reports[0].end, reports[0].file_bytes),
                    (1, 1 + expected, Some(9))
                );
            }
        }
    }
}

#[test]
fn owned_task_rechecks_expiry_after_scheduling_before_source_or_output() {
    let (source, mut articles) = fixture();
    let (mut sink, mut control) = (VecDeliveredSink::default(), Control::new());
    let owned = authorize_original(&request(None), SECRET, NOW)
        .unwrap()
        .prepare_owned(source, None, &mut control)
        .unwrap();
    control.now.set(NOW + 60);
    let st = shared([]);
    let mut future =
        Box::pin(owned.pump(&mut articles, &mut control, &mut sink, Writer(st.clone())));
    assert_eq!(
        future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Ready(Err(SourceError::Cancelled))
    );
    drop(future);
    assert!(articles.opened.is_empty());
    assert!(sink.0.is_empty());
    assert_eq!((st.borrow().aborts, st.borrow().write_polls), (1, 0));
}

#[test]
fn partial_writes_complete_full_and_range_output_then_close_once() {
    for (range, expected) in [(None, 0..9u8), (Some("bytes=2-6"), 2..7)] {
        let (source, mut articles) = fixture();
        let (mut sink, mut control) = (VecDeliveredSink::default(), Control::new());
        let st = shared([Step::Accept(1), Step::Accept(2), Step::Accept(1)]);
        let mut fut = pump!(source, range, articles, control, sink, st);
        let mut cx = Context::from_waker(Waker::noop());
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
        drop(fut);
        let st = st.borrow();
        assert_eq!(st.got, expected.clone().collect::<Vec<_>>());
        assert_eq!((st.closes, st.aborts, st.at_close), (1, 0, st.got.len()));
        assert_eq!(sink.0.len(), 1);
        assert_eq!(
            (sink.0[0].start, sink.0[0].end),
            (expected.start as u64, expected.end as u64)
        );
    }
}

#[test]
fn writer_failure_zero_or_overlong_ack_stops_pump_with_partial_interval() {
    for bad in [Step::Fail, Step::Zero, Step::Over] {
        let (source, mut articles) = fixture();
        let (mut sink, mut control) = (VecDeliveredSink::default(), Control::new());
        let st = shared([Step::Accept(1), bad]);
        let mut fut = pump!(source, None, articles, control, sink, st);
        let mut cx = Context::from_waker(Waker::noop());
        assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Ready(Err(_))));
        drop(fut);
        assert_eq!(
            articles.opened,
            vec![2, 0],
            "no later article after failure"
        );
        let st = st.borrow();
        assert_eq!((st.got.len(), st.closes, st.aborts), (1, 0, 1));
        assert_eq!(sink.0.len(), 1);
        assert_eq!((sink.0[0].start, sink.0[0].end), (0, 1));
    }
}

#[test]
fn pending_write_backpressure_blocks_next_article_until_accepted() {
    let (source, mut articles) = fixture();
    let log = articles.log.clone();
    let (mut sink, mut control) = (VecDeliveredSink::default(), Control::new());
    let st = shared([]);
    st.borrow_mut().block = true;
    let mut fut = pump!(source, None, articles, control, sink, st);
    let mut cx = Context::from_waker(Waker::noop());
    for _ in 0..3 {
        assert!(fut.as_mut().poll(&mut cx).is_pending());
    }
    assert_eq!(
        *log.borrow(),
        vec![2, 0],
        "final/first proof precedes output; backpressure opens no later article"
    );
    assert!(st.borrow().got.is_empty());
    st.borrow_mut().block = false;
    assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
    drop(fut);
    assert_eq!(articles.opened, vec![2, 0, 1]);
    assert_eq!((st.borrow().closes, st.borrow().aborts), (1, 0));
}

#[test]
fn dropping_pending_pump_aborts_once_and_meters_only_prior_ack() {
    for acked in [1usize, 3] {
        let (source, mut articles) = fixture();
        let (mut sink, mut control) = (VecDeliveredSink::default(), Control::new());
        let st = shared([Step::Accept(acked), Step::Block]);
        let mut fut = pump!(source, None, articles, control, sink, st);
        let mut cx = Context::from_waker(Waker::noop());
        assert!(fut.as_mut().poll(&mut cx).is_pending());
        drop(fut);
        let st = st.borrow();
        assert_eq!((st.aborts, st.closes, st.got.len()), (1, 0, acked));
        assert_eq!(sink.0.len(), 1);
        assert_eq!((sink.0[0].start, sink.0[0].end), (0, acked as u64));
    }
}

#[test]
fn wakeable_writer_cancellation_interrupts_pending_article() {
    let (source, mut articles) = fixture();
    articles.pending = Some(2);
    let (mut sink, mut control) = (VecDeliveredSink::default(), Control::new());
    let st = shared([]);
    let wakes = Arc::new(Wakes::default());
    let waker = Waker::from(wakes.clone());
    let mut cx = Context::from_waker(&waker);
    let mut fut = pump!(source, None, articles, control, sink, st);
    assert!(fut.as_mut().poll(&mut cx).is_pending());
    {
        let mut s = st.borrow_mut();
        s.cancel = Some(SourceError::Cancelled);
        s.waker
            .take()
            .expect("poll_cancel registered a waker")
            .wake();
    }
    assert_eq!(wakes.0.load(Ordering::SeqCst), 1);
    assert_eq!(
        fut.as_mut().poll(&mut cx),
        Poll::Ready(Err(SourceError::Cancelled))
    );
    drop(fut);
    assert_eq!(articles.opened, vec![2]);
    let st = st.borrow();
    assert_eq!((st.write_polls, st.closes, st.aborts), (0, 0, 1));
    assert!(sink.0.is_empty());
}

#[test]
fn expiry_while_writer_pending_interrupts_before_next_write_poll() {
    let (source, mut articles) = fixture();
    let (mut sink, mut control) = (VecDeliveredSink::default(), Control::new());
    let now = control.now.clone();
    let st = shared([Step::Accept(3), Step::Block]);
    let mut fut = pump!(source, None, articles, control, sink, st);
    let mut cx = Context::from_waker(Waker::noop());
    assert!(fut.as_mut().poll(&mut cx).is_pending()); // ack 3, then blocked
    assert_eq!(st.borrow().got.len(), 3);
    let polls = st.borrow().write_polls;
    now.set(NOW + 60);
    assert_eq!(
        fut.as_mut().poll(&mut cx),
        Poll::Ready(Err(SourceError::Cancelled))
    );
    drop(fut);
    let st = st.borrow();
    assert_eq!(
        st.write_polls, polls,
        "expired before polling the writer again"
    );
    assert_eq!((st.closes, st.aborts), (0, 1));
    assert_eq!((sink.0[0].start, sink.0[0].end), (0, 3));
}

#[test]
fn dropping_unpolled_pump_aborts_without_source_or_write_io() {
    let (source, mut articles) = fixture();
    let (mut sink, mut control) = (VecDeliveredSink::default(), Control::new());
    let st = shared([]);
    let fut = pump!(source, None, articles, control, sink, st);
    drop(fut);
    assert!(articles.opened.is_empty());
    assert!(sink.0.is_empty());
    let st = st.borrow();
    assert_eq!((st.aborts, st.closes, st.write_polls), (1, 0, 0));
}

#[test]
fn disconnect_wins_a_ready_writer_race_without_acknowledging_its_chunk() {
    let (source, mut articles) = fixture();
    let (mut sink, mut control) = (VecDeliveredSink::default(), Control::new());
    let st = shared([Step::Accept(1), Step::Block]);
    let mut fut = pump!(source, None, articles, control, sink, st);
    let mut cx = Context::from_waker(Waker::noop());
    assert!(fut.as_mut().poll(&mut cx).is_pending());
    let polls = st.borrow().write_polls;
    {
        let mut state = st.borrow_mut();
        state.block = false;
        state.cancel = Some(SourceError::Cancelled);
        state.waker.take().unwrap().wake();
    }
    assert_eq!(
        fut.as_mut().poll(&mut cx),
        Poll::Ready(Err(SourceError::Cancelled))
    );
    drop(fut);
    let st = st.borrow();
    assert_eq!((st.got.len(), st.write_polls, st.aborts), (1, polls, 1));
    assert_eq!(articles.opened, vec![2, 0]);
    assert_eq!((sink.0[0].start, sink.0[0].end), (0, 1));
}

#[test]
fn failed_article_aborts_writer_and_preserves_only_the_acknowledged_prefix() {
    let (source, mut articles) = fixture();
    articles.fail = Some(1);
    let (mut sink, mut control) = (VecDeliveredSink::default(), Control::new());
    let st = shared([]);
    let mut fut = pump!(source, None, articles, control, sink, st);
    let mut cx = Context::from_waker(Waker::noop());
    assert_eq!(
        fut.as_mut().poll(&mut cx),
        Poll::Ready(Err(SourceError::Transport))
    );
    drop(fut);
    let st = st.borrow();
    assert_eq!((st.got.len(), st.aborts, st.closes), (3, 1, 0));
    assert_eq!(articles.opened, vec![2, 0, 1]);
    assert_eq!(sink.0.len(), 1);
    assert_eq!((sink.0[0].start, sink.0[0].end), (0, 3));
}

#[test]
fn control_events_wake_pending_articles_without_retry_or_extra_delivery() {
    for event in [ControlEvent::Cancel, ControlEvent::Expire] {
        for pending in [2, 1] {
            let (source, mut articles) = fixture();
            articles.pending = Some(pending);
            let (mut sink, mut control) = (VecDeliveredSink::default(), Control::new());
            let (now, signal) = (control.now.clone(), control.state.clone());
            let st = shared([]);
            let wakes = Arc::new(Wakes::default());
            let waker = Waker::from(wakes.clone());
            let mut cx = Context::from_waker(&waker);
            let mut fut = pump!(source, Some("bytes=1-7"), articles, control, sink, st);
            assert!(fut.as_mut().poll(&mut cx).is_pending());
            assert_eq!(wakes.0.load(Ordering::SeqCst), 0);
            let polls = st.borrow().write_polls;
            event.fire(&now, &signal);
            assert_eq!(wakes.0.load(Ordering::SeqCst), 1);
            assert_eq!(
                fut.as_mut().poll(&mut cx),
                Poll::Ready(Err(SourceError::Cancelled))
            );
            drop(fut);
            let st = st.borrow();
            assert_eq!((st.write_polls, st.closes, st.aborts), (polls, 0, 1));
            if pending == 2 {
                assert_eq!(articles.opened, vec![2]);
                assert!(st.got.is_empty() && sink.0.is_empty());
            } else {
                assert_eq!(articles.opened, vec![2, 0, 1]);
                assert_eq!(st.got, vec![1, 2]);
                assert_eq!(sink.0.len(), 1);
                assert_eq!((sink.0[0].start, sink.0[0].end), (1, 3));
            }
        }
    }
}

#[test]
fn control_events_win_ready_writer_races_without_another_write_poll() {
    for event in [ControlEvent::Cancel, ControlEvent::Expire] {
        let (source, mut articles) = fixture();
        let (mut sink, mut control) = (VecDeliveredSink::default(), Control::new());
        let (now, signal) = (control.now.clone(), control.state.clone());
        let st = shared([Step::Accept(1), Step::Block]);
        let wakes = Arc::new(Wakes::default());
        let waker = Waker::from(wakes.clone());
        let mut cx = Context::from_waker(&waker);
        let mut fut = pump!(source, Some("bytes=1-7"), articles, control, sink, st);
        assert!(fut.as_mut().poll(&mut cx).is_pending());
        let polls = st.borrow().write_polls;
        st.borrow_mut().block = false;
        event.fire(&now, &signal);
        assert_eq!(wakes.0.load(Ordering::SeqCst), 1);
        assert_eq!(
            fut.as_mut().poll(&mut cx),
            Poll::Ready(Err(SourceError::Cancelled))
        );
        drop(fut);
        let st = st.borrow();
        assert_eq!((st.write_polls, st.closes, st.aborts), (polls, 0, 1));
        assert_eq!(st.got, vec![1]);
        assert_eq!(articles.opened, vec![2, 0]);
        assert_eq!(sink.0.len(), 1);
        assert_eq!((sink.0[0].start, sink.0[0].end), (1, 2));
    }
}

#[test]
fn control_events_wake_pending_close_and_win_ready_close_races() {
    for event in [ControlEvent::Cancel, ControlEvent::Expire] {
        for close_ready in [false, true] {
            let (source, mut articles) = fixture();
            let (mut sink, mut control) = (VecDeliveredSink::default(), Control::new());
            let (now, signal) = (control.now.clone(), control.state.clone());
            let st = shared([]);
            st.borrow_mut().close_block = true;
            let wakes = Arc::new(Wakes::default());
            let waker = Waker::from(wakes.clone());
            let mut cx = Context::from_waker(&waker);
            let mut fut = pump!(source, Some("bytes=1-7"), articles, control, sink, st);
            assert!(fut.as_mut().poll(&mut cx).is_pending());
            assert_eq!(st.borrow().closes, 1);
            let polls = st.borrow().write_polls;
            st.borrow_mut().close_block = !close_ready;
            event.fire(&now, &signal);
            assert_eq!(wakes.0.load(Ordering::SeqCst), 1);
            assert_eq!(
                fut.as_mut().poll(&mut cx),
                Poll::Ready(Err(SourceError::Cancelled))
            );
            drop(fut);
            let st = st.borrow();
            assert_eq!((st.write_polls, st.closes, st.aborts), (polls, 1, 1));
            assert_eq!(st.got, (1..8).collect::<Vec<u8>>());
            assert_eq!(articles.opened, vec![2, 0, 1]);
            assert_eq!(sink.0.len(), 1);
            assert_eq!((sink.0[0].start, sink.0[0].end), (1, 8));
        }
    }
}

#[test]
fn writer_disconnect_wakes_pending_close_with_the_latest_waker() {
    for close_ready in [false, true] {
        let (source, mut articles) = fixture();
        let (mut sink, mut control) = (VecDeliveredSink::default(), Control::new());
        let st = shared([]);
        st.borrow_mut().close_block = true;
        let old_wakes = Arc::new(Wakes::default());
        let old_waker = Waker::from(old_wakes.clone());
        let wakes = Arc::new(Wakes::default());
        let waker = Waker::from(wakes.clone());
        let mut cx = Context::from_waker(&waker);
        let mut fut = pump!(source, Some("bytes=1-7"), articles, control, sink, st);
        assert!(fut
            .as_mut()
            .poll(&mut Context::from_waker(&old_waker))
            .is_pending());
        assert!(fut.as_mut().poll(&mut cx).is_pending());
        let polls = st.borrow().write_polls;
        let closes = st.borrow().closes;
        {
            let mut state = st.borrow_mut();
            state.close_block = !close_ready;
            state.cancel = Some(SourceError::Cancelled);
            state
                .waker
                .take()
                .expect("writer cancellation registered a waker")
                .wake();
        }
        assert_eq!(old_wakes.0.load(Ordering::SeqCst), 0);
        assert_eq!(wakes.0.load(Ordering::SeqCst), 1);
        assert_eq!(
            fut.as_mut().poll(&mut cx),
            Poll::Ready(Err(SourceError::Cancelled))
        );
        drop(fut);
        let st = st.borrow();
        assert_eq!((st.write_polls, st.closes, st.aborts), (polls, closes, 1));
        assert_eq!(st.got, (1..8).collect::<Vec<u8>>());
        assert_eq!(articles.opened, vec![2, 0, 1]);
        assert_eq!(sink.0.len(), 1);
        assert_eq!((sink.0[0].start, sink.0[0].end), (1, 8));
    }
}

#[test]
fn pending_close_resumes_on_writer_wake_or_aborts_on_drop() {
    for complete in [false, true] {
        let (source, mut articles) = fixture();
        let (mut sink, mut control) = (VecDeliveredSink::default(), Control::new());
        let st = shared([]);
        st.borrow_mut().close_block = true;
        let wakes = Arc::new(Wakes::default());
        let waker = Waker::from(wakes.clone());
        let mut cx = Context::from_waker(&waker);
        let mut fut = pump!(source, None, articles, control, sink, st);
        assert!(fut.as_mut().poll(&mut cx).is_pending());
        assert_eq!(st.borrow().closes, 1);
        let polls = st.borrow().write_polls;
        if complete {
            let waker = {
                let mut state = st.borrow_mut();
                state.close_block = false;
                state
                    .close_waker
                    .take()
                    .expect("pending close registered its own waker")
            };
            waker.wake();
            assert_eq!(wakes.0.load(Ordering::SeqCst), 1);
            assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
        }
        drop(fut);
        let st = st.borrow();
        assert_eq!(st.write_polls, polls);
        assert_eq!(
            (st.closes, st.aborts),
            if complete { (2, 0) } else { (1, 1) }
        );
        assert_eq!(st.got, (0..9).collect::<Vec<u8>>());
        assert_eq!(articles.opened, vec![2, 0, 1]);
        assert_eq!(sink.0.len(), 1);
        assert_eq!((sink.0[0].start, sink.0[0].end), (0, 9));
    }
}

#[test]
fn control_replaces_the_waker_in_each_pending_phase() {
    enum Phase {
        Article,
        Write,
        Close,
    }
    for event in [ControlEvent::Cancel, ControlEvent::Expire] {
        for phase in [Phase::Article, Phase::Write, Phase::Close] {
            let (source, mut articles) = fixture();
            let (mut sink, mut control) = (VecDeliveredSink::default(), Control::new());
            let (now, signal) = (control.now.clone(), control.state.clone());
            let st = shared([]);
            let (acked, opened) = match phase {
                Phase::Article => {
                    articles.pending = Some(2);
                    (0, vec![2])
                }
                Phase::Write => {
                    st.borrow_mut().script = [Step::Accept(1), Step::Block].into();
                    (1, vec![2, 0])
                }
                Phase::Close => {
                    st.borrow_mut().close_block = true;
                    (9, vec![2, 0, 1])
                }
            };
            let old_wakes = Arc::new(Wakes::default());
            let old_waker = Waker::from(old_wakes.clone());
            let wakes = Arc::new(Wakes::default());
            let waker = Waker::from(wakes.clone());
            let mut cx = Context::from_waker(&waker);
            let mut fut = pump!(source, None, articles, control, sink, st);
            assert!(fut
                .as_mut()
                .poll(&mut Context::from_waker(&old_waker))
                .is_pending());
            assert!(fut.as_mut().poll(&mut cx).is_pending());
            event.fire(&now, &signal);
            assert_eq!(old_wakes.0.load(Ordering::SeqCst), 0);
            assert_eq!(wakes.0.load(Ordering::SeqCst), 1);
            assert_eq!(
                fut.as_mut().poll(&mut cx),
                Poll::Ready(Err(SourceError::Cancelled))
            );
            drop(fut);
            assert_eq!(articles.opened, opened);
            assert_eq!(st.borrow().aborts, 1);
            assert_eq!(st.borrow().got.len(), acked);
            if acked == 0 {
                assert!(sink.0.is_empty());
            } else {
                assert_eq!(sink.0.len(), 1);
                assert_eq!((sink.0[0].start, sink.0[0].end), (0, acked as u64));
            }
        }
    }
}

#[test]
fn request_control_relay_cancels_each_pending_phase_with_latest_task_and_exact_ack() {
    for cancel in [true, false] {
        for phase in 0..3 {
            let (source, mut articles) = fixture();
            let mut sink = VecDeliveredSink::default();
            let ready = Rc::new(Cell::new(false));
            let timer_waker = Rc::new(RefCell::new(None::<Waker>));
            let timer = std::future::poll_fn({
                let (ready, timer_waker) = (ready.clone(), timer_waker.clone());
                move |cx| {
                    // Like the pinned SDK, intentionally retain the FIRST waker.
                    timer_waker
                        .borrow_mut()
                        .get_or_insert_with(|| cx.waker().clone());
                    if ready.get() {
                        Poll::Ready(())
                    } else {
                        Poll::Pending
                    }
                }
            });
            let mut control = OriginalRequestControl::new(NOW + 60, timer, || NOW);
            let cancellation = control.cancellation();
            let st = shared([]);
            let (acked, opened) = match phase {
                0 => {
                    articles.pending = Some(2);
                    (0, vec![2])
                }
                1 => {
                    st.borrow_mut().script = [Step::Accept(1), Step::Block].into();
                    (1, vec![2, 0])
                }
                _ => {
                    st.borrow_mut().close_block = true;
                    (9, vec![2, 0, 1])
                }
            };
            let old = Arc::new(Wakes::default());
            let latest = Arc::new(Wakes::default());
            let old_waker = Waker::from(old.clone());
            let latest_waker = Waker::from(latest.clone());
            let mut cx = Context::from_waker(&latest_waker);
            let mut fut = pump!(source, None, articles, control, sink, st);
            assert!(fut
                .as_mut()
                .poll(&mut Context::from_waker(&old_waker))
                .is_pending());
            assert!(fut.as_mut().poll(&mut cx).is_pending());
            let polls = (st.borrow().write_polls, st.borrow().closes);
            if cancel {
                cancellation.cancel();
            } else {
                ready.set(true);
                timer_waker.borrow_mut().take().unwrap().wake();
            }
            assert_eq!(old.0.load(Ordering::SeqCst), 0);
            assert_eq!(latest.0.load(Ordering::SeqCst), 1);
            assert_eq!(
                fut.as_mut().poll(&mut cx),
                Poll::Ready(Err(SourceError::Cancelled))
            );
            drop(fut);
            assert_eq!(articles.opened, opened);
            assert_eq!((st.borrow().write_polls, st.borrow().closes), polls);
            assert_eq!(st.borrow().aborts, 1);
            assert_eq!(st.borrow().got.len(), acked);
            if acked == 0 {
                assert!(sink.0.is_empty());
            } else {
                assert_eq!(sink.0.len(), 1);
                assert_eq!((sink.0[0].start, sink.0[0].end), (0, acked as u64));
            }
        }
    }
}

#[test]
fn close_failure_keeps_full_ack_interval_and_aborts() {
    let (source, mut articles) = fixture();
    let (mut sink, mut control) = (VecDeliveredSink::default(), Control::new());
    let st = shared([]);
    st.borrow_mut().close = Some(SourceError::Transport);
    let mut fut = pump!(source, None, articles, control, sink, st);
    let mut cx = Context::from_waker(Waker::noop());
    assert_eq!(
        fut.as_mut().poll(&mut cx),
        Poll::Ready(Err(SourceError::Transport))
    );
    drop(fut);
    let st = st.borrow();
    assert_eq!((st.got.len(), st.closes, st.aborts), (9, 1, 1));
    assert_eq!(sink.0.len(), 1);
    assert_eq!((sink.0[0].start, sink.0[0].end), (0, 9));
}
