//! Synthetic byte patterns only. Off-route tests, not Worker/provider proof.
use std::{
    cell::Cell,
    future::Future,
    rc::Rc,
    task::{Context, Poll, Waker},
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
    original_play::{authorize_original, OriginalControl},
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

struct Control {
    now: Rc<Cell<u64>>,
    cancelled: bool,
}
impl Control {
    fn new() -> Self {
        Self {
            now: Rc::new(Cell::new(NOW)),
            cancelled: false,
        }
    }
}
impl ReadControl for Control {
    fn check(&mut self) -> Result<(), SourceError> {
        if self.cancelled {
            Err(SourceError::Cancelled)
        } else {
            Ok(())
        }
    }
}
impl AsyncReadControl for Control {
    fn poll_cancel(&mut self, _: &mut Context<'_>) -> Poll<SourceError> {
        if self.cancelled {
            Poll::Ready(SourceError::Cancelled)
        } else {
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
}
impl AsyncArticleSource for Articles {
    async fn open(&mut self, index: usize, reference: &str) -> Result<Vec<u8>, SourceError> {
        assert_eq!(reference, format!("generated-{index}@fixture.invalid"));
        self.opened.push(index);
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
        Some(grant::issue_v2(
            "user_generated",
            "other",
            "play_generated",
            1,
            NOW + 60,
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
        assert_eq!(
            authority
                .prepare(&source, Some(range), &mut control, &mut sink)
                .err()
                .unwrap()
                .status,
            416
        );
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
