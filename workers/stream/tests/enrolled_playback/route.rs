use super::*;
use nntune_core::{digest_readback::ReadbackPin, original_source::SourceError};
use nntune_stream::{
    delivered::DeliveredInterval,
    enrolled_playback::{
        route::{serve, CatalogueMedia, PrivateSource},
        S3Get, S3GetReply,
    },
    grant::Grant,
    meter_sink::{FlushAck, MeterBook},
    original_play::OriginalWriter,
    play_route::PlayRouteRequest,
};
use std::{
    cell::{Cell, RefCell},
    io,
    rc::Rc,
    task::{Context, Poll},
};

#[path = "../fixtures/private_enrolled_source.rs"]
mod fixture;
use fixture::{Fault, FrameGate, Source, Writer, FLAC};

const TOKEN: &str = include_str!("../../../../fixtures/play-grant/private-enrolled.token");
// Pre-signed synthetic v4 grant for Fixture::new(), expiring at Unix 12.
const BROWSER_TOKEN: &str = concat!(
    "v4.bWVtYmVyfHRyYWNrfHByaXZhdGVfYnJvd3Nlcl9BQUFBQUFBQUFBQUFBQUFBQUFBQUFBfDQyfDEyf",
    "GNmMjQ0ZjZkYzYxYWI3ZTU3MTI4MzYwODM2NGZmZGViZTAzZjBiNGZmMGE2MDcyNDE5MDFmOTUzZWE5MDY2ZGE.",
    "5OZv4_yGbRpUk6HYDXTckBYY046ImHeyN71sQOoZ0MA",
);
// Synthetic v4 grant for Source::other_track(), also expiring at Unix 12.
const OTHER_TRACK_TOKEN: &str = concat!(
    "v4.bWVtYmVyfG90aGVyLXRyYWNrfHByaXZhdGVfYnJvd3Nlcl9CQkJCQkJCQkJCQkJCQkJCQkJCQkJCfDQzfDEy",
    "fDU3ODZjZjBjNjhlNTBiZjg1MjU3NTk2NTU3ZTJhZjZkN2Q2YWNkZWFiZDRlYTJiYjNlMDkzNjQxNzRjMmIwZTc.",
    "zgFXxaz9HOuIYHMDUw_YXnXqvSqS-9aEL81H7XiHygY",
);

#[tokio::test]
async fn interrupting_one_track_preserves_the_other_tracks_cursor_and_receipt() {
    use nntune_stream::{delivered::DeliveredInterval, meter_sink::MeterBook};

    let request = |track: &str, token: &str| PlayRouteRequest {
        track_id: track.into(),
        token: Some(token.into()),
        range: Some("bytes=65534-65536".into()),
        trace_id: None,
    };
    let interrupted_request = request("track", BROWSER_TOKEN);
    let complete_request = request("other-track", OTHER_TRACK_TOKEN);
    let current_epoch = Rc::new(Cell::new(42));
    let interrupted_source = Source::browser().with_current_epoch(42, current_epoch.clone());
    let complete_source = Source::other_track().with_current_epoch(43, current_epoch.clone());
    let interrupted_gate = Rc::<FrameGate>::default();
    let complete_gate = Rc::<FrameGate>::default();
    interrupted_source
        .pause_final_frame
        .set(Some(interrupted_gate.clone()));
    complete_source
        .pause_final_frame
        .set(Some(complete_gate.clone()));
    let interrupted_output = RefCell::new(Vec::new());
    let complete_output = RefCell::new(Vec::new());
    let interrupted_aborted = Cell::new(false);
    let complete_aborted = Cell::new(false);
    let reports = RefCell::new(Vec::new());
    let meter = RefCell::new(MeterBook::new("member"));
    let (report_log, delivery_meter) = (&reports, &meter);
    let start_read = |req, source, output, aborted| {
        serve(
            req,
            b"test only, not a deployed secret",
            Some(source),
            now,
            move |head| {
                assert_eq!(head.status, 206);
                assert_eq!(head.len, 3);
                assert_eq!(
                    head.content_range.as_deref(),
                    Some("bytes 65534-65536/65537")
                );
                Ok(Some(Writer {
                    output,
                    fault: Fault::None,
                    aborted,
                    closed: None,
                    revoke_on_write: None,
                }))
            },
            move |receipt| {
                assert!(delivery_meter.borrow_mut().record(&receipt, 10));
                report_log.borrow_mut().push(receipt);
            },
        )
    };
    let mut interrupted = Box::pin(start_read(
        &interrupted_request,
        &interrupted_source,
        &interrupted_output,
        &interrupted_aborted,
    ));
    assert!(futures_util::poll!(&mut interrupted).is_pending());
    assert!(interrupted_gate.waiting.get());
    assert_eq!(*interrupted_output.borrow(), b"AY");

    // The next track replaces the lease while the old read is still pending.
    current_epoch.set(43);
    let mut complete = Box::pin(start_read(
        &complete_request,
        &complete_source,
        &complete_output,
        &complete_aborted,
    ));
    assert!(futures_util::poll!(&mut complete).is_pending());
    assert!(complete_gate.waiting.get());
    assert_eq!(*complete_output.borrow(), b"BQ");
    assert!(reports.borrow().is_empty());

    let receipt = |track: &str, play: &str, end| DeliveredInterval {
        user_id: "member".into(),
        track_id: track.into(),
        play_id: play.into(),
        start: 65_534,
        end,
        file_bytes: Some(LEN),
    };
    let interrupted_play = "private_browser_AAAAAAAAAAAAAAAAAAAAAA";
    let complete_play = "private_browser_BBBBBBBBBBBBBBBBBBBBBB";
    let interrupted_receipt = receipt("track", interrupted_play, 65_536);

    drop(interrupted);
    assert!(interrupted_gate.dropped.get());
    assert!(!complete_gate.dropped.get());
    assert_eq!(
        reports.borrow().as_slice(),
        std::slice::from_ref(&interrupted_receipt)
    );

    complete_gate.released.set(true);
    assert!(matches!(
        futures_util::poll!(&mut complete),
        Poll::Ready(Ok(()))
    ));
    assert!(complete_gate.dropped.get());
    assert!(!complete_aborted.get());
    assert_eq!(*interrupted_output.borrow(), b"AY");
    assert_eq!(*complete_output.borrow(), b"BQR");
    assert_eq!(
        *reports.borrow(),
        [
            interrupted_receipt,
            receipt("other-track", complete_play, LEN)
        ]
    );
    let meter = meter.borrow();
    assert_eq!(meter.plays.len(), 2);
    assert_eq!(meter.plays[interrupted_play].union.total_bytes(), 2);
    assert_eq!(meter.plays[complete_play].union.total_bytes(), 3);
}

#[tokio::test]
async fn private_route_authenticates_flac_and_accounts_for_writer_prefixes() {
    let key = b"test only, not a deployed secret";
    assert!(nntune_stream::grant::verify(TOKEN, key, 10).is_err());
    for case in fixture::cases() {
        let source = Source::new(case.fault);
        let track = if case.fault == Fault::WrongTrack {
            "other"
        } else {
            "track"
        };
        let req = PlayRouteRequest {
            track_id: track.into(),
            token: Some(TOKEN.into()),
            range: case.range.map(str::to_owned),
            trace_id: None,
        };
        let output = RefCell::new(Vec::new());
        let aborted = Cell::new(false);
        let mut status = 0;
        let mut report = None;
        let outcome = serve(
            &req,
            key,
            (case.fault != Fault::Off).then_some(&source),
            || source.time(),
            |head| {
                status = head.status;
                assert_eq!(head.content_range.as_deref(), case.content_range);
                let len = match status {
                    206 => case.delivered.len(),
                    416 => 0,
                    _ => FLAC.len(),
                };
                assert_eq!(head.len, len as u64);
                Ok((status != 416).then_some(Writer {
                    output: &output,
                    fault: case.fault,
                    aborted: &aborted,
                    closed: None,
                    revoke_on_write: None,
                }))
            },
            |interval| report = Some(interval),
        )
        .await;
        assert_eq!(
            outcome.map_or_else(|error| error.status, |()| status),
            case.status,
            "{:?}",
            case.fault
        );
        assert_eq!(
            status,
            if case.fault == Fault::ExpiredAtWrite {
                0
            } else {
                case.response_status()
            }
        );
        assert_eq!(aborted.get(), case.fault == Fault::Writer);
        let slice = case.delivered;
        assert_eq!(output.into_inner(), FLAC[slice.clone()]);
        assert_eq!(
            report.map(|r| r.start as usize..r.end as usize),
            (!slice.is_empty()).then_some(slice)
        );
    }
}

struct DeadlineSource {
    inner: Source,
    snapshot_until: SystemTime,
    expected_cutoff: SystemTime,
}

impl PrivateSource for DeadlineSource {
    type Body = <Source as PrivateSource>::Body;

    async fn resolve(&self, grant: &Grant) -> Option<CatalogueMedia<'_>> {
        let mut resolved = self.inner.resolve(grant).await?;
        resolved.pins.usable_until = self.snapshot_until;
        Some(resolved)
    }

    async fn lease_live(&self, grant: &Grant) -> bool {
        self.inner.lease_live(grant).await
    }

    async fn media(&self, get: S3Get<'_>) -> io::Result<S3GetReply<Self::Body>> {
        assert_eq!(get.usable_until, self.expected_cutoff);
        self.inner.media(get).await
    }

    async fn page(
        &self,
        bucket: &str,
        pin: ReadbackPin<'_>,
        length: u32,
        until: SystemTime,
    ) -> io::Result<(String, Vec<u8>)> {
        assert_eq!(until, self.expected_cutoff);
        self.inner
            .page(bucket, pin, length, now() + Duration::from_secs(1))
            .await
    }
}

#[derive(Default)]
struct Sink {
    output: RefCell<Vec<u8>>,
    writable: Cell<bool>,
    close_polled: Cell<bool>,
    disconnected: Cell<bool>,
    aborted: Cell<bool>,
}

impl OriginalWriter for &Sink {
    fn poll_cancel(&mut self, _: &mut Context<'_>) -> Poll<SourceError> {
        if self.disconnected.get() {
            Poll::Ready(SourceError::Cancelled)
        } else {
            Poll::Pending
        }
    }

    fn poll_write(
        &mut self,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<Result<usize, SourceError>> {
        if self.writable.replace(false) {
            self.output.borrow_mut().push(bytes[0]);
            Poll::Ready(Ok(1))
        } else {
            Poll::Pending
        }
    }

    fn poll_close(&mut self, _: &mut Context<'_>) -> Poll<Result<(), SourceError>> {
        self.close_polled.set(true);
        Poll::Pending
    }

    fn abort(&mut self) {
        self.aborted.set(true);
    }
}

#[tokio::test]
async fn pending_writes_credit_only_acknowledged_bytes_on_expiry_or_drop() {
    let request = PlayRouteRequest {
        track_id: "track".into(),
        token: Some(TOKEN.into()),
        range: Some("bytes=-2".into()),
        trace_id: None,
    };
    // The fixture grant expires at Unix 12. Exercise the earlier cutoff in both orders.
    for (snapshot_secs, cutoff_secs, expire) in [(1, 1, true), (3, 2, true), (3, 2, false)] {
        let source = DeadlineSource {
            inner: Source::new(Fault::None),
            snapshot_until: now() + Duration::from_secs(snapshot_secs),
            expected_cutoff: now() + Duration::from_secs(cutoff_secs),
        };
        let clock = Cell::new(now());
        let sink = Sink::default();
        sink.writable.set(true);
        let reports = RefCell::new(Vec::new());
        let mut play = Box::pin(serve(
            &request,
            b"test only, not a deployed secret",
            Some(&source),
            || clock.get(),
            |head| {
                assert_eq!((head.status, head.len), (206, 2));
                Ok(Some(&sink))
            },
            |interval| reports.borrow_mut().push(interval),
        ));
        assert!(futures_util::poll!(&mut play).is_pending());
        assert_eq!(*sink.output.borrow(), FLAC[FLAC.len() - 2..FLAC.len() - 1]);
        assert!(reports.borrow().is_empty());

        // Make the second write ready. At the exact cutoff it must remain unacknowledged.
        sink.writable.set(true);
        if expire {
            clock.set(source.expected_cutoff);
            assert!(
                matches!(futures_util::poll!(&mut play), Poll::Ready(Err(error)) if error.status == 502)
            );
            assert!(sink.aborted.get());
        }
        drop(play);

        assert!(sink.writable.get());
        assert!(!sink.close_polled.get());
        let reports = reports.into_inner();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].start..reports[0].end, 51..52);
        assert_eq!(reports[0].file_bytes, Some(FLAC.len() as u64));
    }
}

#[tokio::test]
async fn pending_close_aborts_on_disconnect_or_expiry_and_preserves_acknowledgement() {
    let request = PlayRouteRequest {
        track_id: "track".into(),
        token: Some(TOKEN.into()),
        range: Some("bytes=-1".into()),
        trace_id: None,
    };
    for disconnect in [true, false] {
        let source = Source::new(Fault::None);
        let clock = Cell::new(now());
        let sink = Sink::default();
        sink.writable.set(true);
        let mut report = None;
        let mut play = Box::pin(serve(
            &request,
            b"test only, not a deployed secret",
            Some(&source),
            || clock.get(),
            |_| Ok(Some(&sink)),
            |interval| report = Some(interval),
        ));
        assert!(futures_util::poll!(&mut play).is_pending());
        assert!(sink.close_polled.get());
        if disconnect {
            sink.disconnected.set(true);
        } else {
            clock.set(now() + Duration::from_secs(1));
        }
        assert!(
            matches!(futures_util::poll!(&mut play), Poll::Ready(Err(error)) if error.status == 502)
        );
        assert!(sink.aborted.get());
        drop(play);
        assert_eq!(*sink.output.borrow(), FLAC[FLAC.len() - 1..]);
        let report = report.unwrap();
        assert_eq!(report.start..report.end, 52..53);
    }
}

#[tokio::test]
async fn open_ended_range_delivers_plaintext_through_eof() {
    let source = Source::browser();
    let request = PlayRouteRequest {
        track_id: "track".into(),
        token: Some(BROWSER_TOKEN.into()),
        range: Some("bytes=65534-".into()),
        trace_id: None,
    };
    let output = RefCell::new(Vec::new());
    let aborted = Cell::new(false);
    let mut delivered = None;
    serve(
        &request,
        b"test only, not a deployed secret",
        Some(&source),
        || source.time(),
        |head| {
            assert_eq!(head.status, 206);
            assert_eq!(head.len, 3);
            assert_eq!(
                head.content_range.as_deref(),
                Some("bytes 65534-65536/65537")
            );
            Ok(Some(Writer {
                output: &output,
                fault: Fault::None,
                aborted: &aborted,
                closed: None,
                revoke_on_write: None,
            }))
        },
        |interval| delivered = Some(interval),
    )
    .await
    .unwrap();

    assert!(!aborted.get());
    assert_eq!(output.into_inner(), b"AYZ");
    let receipt = delivered.expect("successful write records delivery");
    assert_eq!(receipt.start..receipt.end, 65_534..LEN);
    assert_eq!(receipt.file_bytes, Some(LEN));
}

#[tokio::test]
async fn empty_media_read_does_not_credit_or_advance_seek() {
    let source = Source::browser();
    source.expected_media_start.set(65_600);
    let request = PlayRouteRequest {
        track_id: "track".into(),
        token: Some(BROWSER_TOKEN.into()),
        range: Some("bytes=65536-65536".into()),
        trace_id: None,
    };
    let attempt = async || {
        let output = RefCell::new(Vec::new());
        let aborted = Cell::new(false);
        let mut started = false;
        let mut reports = Vec::new();
        let result = serve(
            &request,
            b"test only, not a deployed secret",
            Some(&source),
            || source.time(),
            |head| {
                started = true;
                assert_eq!(head.status, 206);
                assert_eq!(head.len, 1);
                assert_eq!(
                    head.content_range.as_deref(),
                    Some("bytes 65536-65536/65537")
                );
                Ok(Some(Writer {
                    output: &output,
                    fault: Fault::None,
                    aborted: &aborted,
                    closed: None,
                    revoke_on_write: None,
                }))
            },
            |interval| reports.push(interval),
        )
        .await;
        (result, started, output.into_inner(), aborted.get(), reports)
    };

    source.empty_media_once.set(true);
    let (result, started, output, _, reports) = attempt().await;
    let error = result.unwrap_err();
    assert_eq!(error.status, 502);
    assert_eq!(error.code, "upstream_unavailable");
    // The pinned digest page is fetched only after the storage metadata is accepted.
    assert_eq!(source.page_reads.get(), 1);
    assert!(!started);
    assert!(output.is_empty());
    assert!(reports.is_empty());

    let (result, started, output, aborted, reports) = attempt().await;
    result.unwrap();
    assert_eq!(source.page_reads.get(), 2);
    assert!(started);
    assert!(!aborted);
    assert_eq!(output, b"Z");
    assert_eq!(
        reports,
        [nntune_stream::delivered::DeliveredInterval {
            user_id: "member".into(),
            play_id: "private_browser_AAAAAAAAAAAAAAAAAAAAAA".into(),
            track_id: "track".into(),
            start: 65_536,
            end: LEN,
            file_bytes: Some(LEN),
        }]
    );
}

#[tokio::test]
async fn short_media_read_reports_only_written_bytes_and_retry_is_contiguous() {
    let source = Source::browser();
    let attempt = async |start: u64| {
        let request = PlayRouteRequest {
            track_id: "track".into(),
            token: Some(BROWSER_TOKEN.into()),
            range: Some(format!("bytes={start}-65536")),
            trace_id: None,
        };
        let output = RefCell::new(Vec::new());
        let aborted = Cell::new(false);
        let mut receipt = None;
        let result = serve(
            &request,
            b"test only, not a deployed secret",
            Some(&source),
            || source.time(),
            |head| {
                assert_eq!(head.status, 206);
                assert_eq!(head.len, LEN - start);
                Ok(Some(Writer {
                    output: &output,
                    fault: Fault::None,
                    aborted: &aborted,
                    closed: None,
                    revoke_on_write: None,
                }))
            },
            |interval| receipt = Some(interval),
        )
        .await;
        (
            result,
            output.into_inner(),
            aborted.get(),
            receipt.expect("writer-accepted bytes must be reported"),
        )
    };

    source.short_media_once.set(true);
    let start = 65_534;
    let (result, prefix, aborted, receipt) = attempt(start).await;
    assert_eq!(result.unwrap_err().status, 502);
    assert!(aborted);
    assert_eq!(prefix, b"AY");
    let cursor = start + prefix.len() as u64;
    assert_eq!(receipt.start..receipt.end, start..cursor);

    source.expected_media_start.set(65_600);
    let (result, tail, aborted, retry) = attempt(cursor).await;
    result.unwrap();
    assert!(!aborted);
    assert_eq!(tail, b"Z");
    assert_eq!(retry.start..retry.end, cursor..cursor + tail.len() as u64);
    assert_eq!(retry.end, LEN);
    assert_eq!(retry.play_id, receipt.play_id);
}

#[tokio::test]
async fn final_frame_verification_failure_preserves_acknowledged_delivery() {
    let source = Source::browser();
    let play_id = "private_browser_AAAAAAAAAAAAAAAAAAAAAA";
    let mut meter = MeterBook::new("member");
    let terminal = FlushAck {
        accepted: vec![play_id.into()],
        ..FlushAck::default()
    }
    .terminal_play_ids();
    let receipt = |start, end| DeliveredInterval {
        user_id: "member".into(),
        play_id: play_id.into(),
        track_id: "track".into(),
        start,
        end,
        file_bytes: Some(LEN),
    };
    let attempt = async |start: u64, end: u64| {
        let request = PlayRouteRequest {
            track_id: "track".into(),
            token: Some(BROWSER_TOKEN.into()),
            range: Some(format!("bytes={start}-{end}")),
            trace_id: None,
        };
        let content_range = format!("bytes {start}-{end}/{LEN}");
        let output = RefCell::new(Vec::new());
        let aborted = Cell::new(false);
        let mut reports = Vec::new();
        let result = serve(
            &request,
            b"test only, not a deployed secret",
            Some(&source),
            || source.time(),
            |head| {
                assert_eq!(head.status, 206);
                assert_eq!(head.len, end - start + 1);
                assert_eq!(head.content_range.as_deref(), Some(content_range.as_str()));
                Ok(Some(Writer {
                    output: &output,
                    fault: Fault::None,
                    aborted: &aborted,
                    closed: None,
                    revoke_on_write: None,
                }))
            },
            |interval| reports.push(interval),
        )
        .await;
        (result, output.into_inner(), aborted.get(), reports)
    };

    source.expected_media_end.set(65_600);
    let (result, output, aborted, reports) = attempt(65_533, 65_534).await;
    result.unwrap();
    assert!(!aborted);
    assert_eq!(output, b"AA");
    assert_eq!(reports, [receipt(65_533, 65_535)]);
    assert!(meter.record(&reports[0], 10));
    let earlier = meter.pending_windows();
    assert_eq!(earlier.len(), 1);
    assert_eq!(earlier[0].union_bytes, 2);
    meter.mark_flushed(&earlier, &terminal);
    assert_eq!(meter.plays[play_id].flushed_bytes, 2);
    assert!(!meter.has_pending());

    source.expected_media_end.set(65_617);
    source.corrupt_final_frame_once.set(true);
    let (result, output, aborted, reports) = attempt(65_534, 65_536).await;
    let error = result.unwrap_err();
    assert_eq!((error.status, error.code), (502, "upstream_unavailable"));
    assert!(aborted);
    assert_eq!(output, b"AY");
    assert_eq!(reports, [receipt(65_534, 65_536)]);
    assert!(meter.record(&reports[0], 11));
    assert_eq!(meter.plays[play_id].flushed_bytes, 2);
    let prefix = meter.pending_windows();
    assert_eq!(prefix.len(), 1);
    assert_eq!(prefix[0].union_bytes, 3); // One byte overlaps the earlier range.
    meter.mark_flushed(&prefix, &terminal);
    assert_eq!(meter.plays[play_id].flushed_bytes, 3);
    assert!(!meter.has_pending());

    source.expected_media_start.set(65_600);
    let (result, output, aborted, reports) = attempt(65_536, 65_536).await;
    result.unwrap();
    assert!(!aborted);
    assert_eq!(output, b"Z");
    assert_eq!(reports, [receipt(65_536, LEN)]);
    assert!(meter.record(&reports[0], 12));
    let complete = meter.pending_windows();
    assert_eq!(complete.len(), 1);
    assert_eq!(complete[0].union_bytes, 4);
    meter.mark_flushed(&complete, &terminal);
    assert_eq!(meter.plays[play_id].flushed_bytes, 4);
    assert!(!meter.has_pending());
}

#[tokio::test]
async fn browser_stream_aborts_at_the_next_chunk_after_lease_revocation() {
    let key = b"test only, not a deployed secret";
    let source = Source::browser();
    let request = PlayRouteRequest {
        track_id: "track".into(),
        token: Some(BROWSER_TOKEN.into()),
        range: Some("bytes=65535-65536".into()),
        trace_id: None,
    };
    let output = RefCell::new(Vec::new());
    let aborted = Cell::new(false);
    let mut delivered = Vec::new();
    let result = serve(
        &request,
        key,
        Some(&source),
        || source.time(),
        |head| {
            assert_eq!(head.status, 206);
            assert_eq!(head.len, 2);
            assert_eq!(
                head.content_range.as_deref(),
                Some("bytes 65535-65536/65537")
            );
            // The sink accepts the final byte of the first chunk, then
            // the lease is lost before the second chunk can be delivered.
            Ok(Some(Writer {
                output: &output,
                fault: Fault::None,
                aborted: &aborted,
                closed: None,
                revoke_on_write: Some(&source.revoked),
            }))
        },
        |interval| delivered.push(interval),
    )
    .await;
    assert_eq!(result.unwrap_err().status, 404);
    assert!(aborted.get());
    assert_eq!(output.into_inner(), b"Y");
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].start..delivered[0].end, 65_535..65_536);

    let replay = serve::<_, Writer<'_>>(
        &request,
        key,
        Some(&source),
        || source.time(),
        |_| panic!("a revoked bearer started a response"),
        |_| panic!("a refused response recorded delivery"),
    )
    .await;
    assert_eq!(replay.unwrap_err().status, 404);
}

#[tokio::test]
async fn successful_seek_reports_all_written_bytes_and_meters_only_new_bytes() {
    use nntune_stream::{
        delivered::DeliveredInterval,
        meter_sink::{MeterBook, WindowReport},
    };

    let play_id = "private_1234567890abcdefghijkl";
    let file_bytes = Some(FLAC.len() as u64);
    // Both seeks write eight bytes. Only the first seek repeats four bytes.
    for (seek, total) in [(4..12, 12), (8..16, 16)] {
        let source = Source::new(Fault::None);
        let mut meter = MeterBook::new("member");

        for (range, observed_at, union_bytes) in [(0..8, 10, 8), (seek, 11, total)] {
            let last = range.end - 1;
            let request = PlayRouteRequest {
                track_id: "track".into(),
                token: Some(TOKEN.into()),
                range: Some(format!("bytes={}-{last}", range.start)),
                trace_id: None,
            };
            let content_range = format!("bytes {}-{last}/{}", range.start, FLAC.len());
            let output = RefCell::new(Vec::new());
            let aborted = Cell::new(false);
            let mut reports = Vec::new();
            serve(
                &request,
                b"test only, not a deployed secret",
                Some(&source),
                || source.time(),
                |head| {
                    assert_eq!(head.status, 206);
                    assert_eq!(head.len, 8);
                    assert_eq!(head.content_range.as_deref(), Some(content_range.as_str()));
                    Ok(Some(Writer {
                        output: &output,
                        fault: Fault::None,
                        aborted: &aborted,
                        closed: None,
                        revoke_on_write: None,
                    }))
                },
                |receipt| reports.push(receipt),
            )
            .await
            .unwrap();

            assert!(!aborted.get());
            let output = output.into_inner();
            assert_eq!(output, FLAC[range.clone()]);
            let start = range.start as u64;
            let cursor = start + output.len() as u64;
            assert_eq!(
                reports,
                [DeliveredInterval {
                    user_id: "member".into(),
                    play_id: play_id.into(),
                    track_id: "track".into(),
                    start,
                    end: cursor,
                    file_bytes,
                }]
            );
            assert!(meter.record(&reports[0], observed_at));
            assert_eq!(meter.plays.len(), 1);
            assert_eq!(
                meter.pending_windows(),
                [WindowReport {
                    user_id: "member".into(),
                    play_id: play_id.into(),
                    track_id: "track".into(),
                    union_bytes,
                    file_bytes,
                    started_at: 10,
                }],
                "after delivering {range:?}"
            );
        }
    }
}

#[tokio::test]
async fn dropping_a_partial_encrypted_read_leaves_the_other_caller_live() {
    let source = Source::browser();
    let request = |start| PlayRouteRequest {
        track_id: "track".into(),
        token: Some(BROWSER_TOKEN.into()),
        range: Some(format!("bytes={start}-65536")),
        trace_id: None,
    };
    let cancelled = request(65_534);
    let live = request(65_535);
    let cancelled_output = RefCell::new(Vec::new());
    let live_output = RefCell::new(Vec::new());
    let cancelled_closed = Cell::new(false);
    let live_closed = Cell::new(false);
    let cancelled_aborted = Cell::new(false);
    let live_aborted = Cell::new(false);
    let reports = RefCell::new(Vec::new());
    let read = |request, output, closed, aborted, range: &'static str, len| {
        serve(
            request,
            b"test only, not a deployed secret",
            Some(&source),
            || source.time(),
            move |head| {
                assert_eq!(head.status, 206);
                assert_eq!(head.content_range.as_deref(), Some(range));
                assert_eq!(head.len, len);
                Ok(Some(Writer {
                    output,
                    fault: Fault::None,
                    aborted,
                    closed: Some(closed),
                    revoke_on_write: None,
                }))
            },
            |interval| reports.borrow_mut().push(interval),
        )
    };
    let receipt = |start, end| DeliveredInterval {
        user_id: "member".into(),
        play_id: "private_browser_AAAAAAAAAAAAAAAAAAAAAA".into(),
        track_id: "track".into(),
        start,
        end,
        file_bytes: Some(LEN),
    };

    let cancelled_tail = Rc::<FrameGate>::default();
    source.pause_last_byte.set(Some(cancelled_tail.clone()));
    let mut cancelled = Box::pin(read(
        &cancelled,
        &cancelled_output,
        &cancelled_closed,
        &cancelled_aborted,
        "bytes 65534-65536/65537",
        3,
    ));
    assert!(futures_util::poll!(&mut cancelled).is_pending());
    assert!(cancelled_tail.waiting.get());
    assert_eq!(*cancelled_output.borrow(), b"AY");

    let live_tail = Rc::<FrameGate>::default();
    source.pause_last_byte.set(Some(live_tail.clone()));
    let mut live = Box::pin(read(
        &live,
        &live_output,
        &live_closed,
        &live_aborted,
        "bytes 65535-65536/65537",
        2,
    ));
    assert!(futures_util::poll!(&mut live).is_pending());
    assert!(live_tail.waiting.get());
    assert_eq!(*live_output.borrow(), b"Y");
    assert!(reports.borrow().is_empty());

    drop(cancelled);
    assert!(cancelled_tail.dropped.get());
    assert!(!live_tail.dropped.get());
    assert!(!cancelled_closed.get());
    assert!(!live_closed.get());
    assert!(!live_aborted.get());
    assert_eq!(*cancelled_output.borrow(), b"AY");
    assert_eq!(*live_output.borrow(), b"Y");
    assert_eq!(*reports.borrow(), [receipt(65_534, 65_536)]);

    live_tail.released.set(true);
    assert!(matches!(
        futures_util::poll!(&mut live),
        Poll::Ready(Ok(()))
    ));
    assert!(live_tail.dropped.get());
    assert!(live_closed.get());
    assert!(!live_aborted.get());
    assert_eq!(*live_output.borrow(), b"YZ");
    assert_eq!(
        *reports.borrow(),
        [receipt(65_534, 65_536), receipt(65_535, LEN)]
    );
}

#[tokio::test]
async fn operator_source_stays_closed_until_verified_encryption_outputs_and_reopens() {
    use nntune_core::digest_readback::ReadbackPin;
    use nntune_stream::enrolled_playback::{
        route::{CatalogueMedia, PrivateSource},
        S3Get, S3GetReply,
    };

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Stage {
        Closed,
        WrongDigest,
        WrongRoot,
        CorruptEnvelope,
        TruncatedRetained,
        EtagLocator,
        EmptyPages,
        MismatchedMediaVersion,
        NullPageVersion,
        Verified,
    }
    struct ReopenSource {
        inner: Source,
        wrong_root: [u8; 32],
        corrupt: [u8; 230],
        etag_media: S3MediaMetadata,
        null_pages: [String; 1],
        stage: Cell<Stage>,
        media_reads: Cell<usize>,
    }
    impl PrivateSource for ReopenSource {
        type Body = fixture::BodyBytes;
        async fn resolve(&self, g: &nntune_stream::grant::Grant) -> Option<CatalogueMedia<'_>> {
            (self.stage.get() != Stage::Closed).then_some(())?;
            let mut m = self.inner.resolve(g).await?;
            match self.stage.get() {
                Stage::WrongDigest => m.pins.plaintext_sha256[0] ^= 1,
                Stage::WrongRoot => m.media.root = &self.wrong_root,
                Stage::CorruptEnvelope => {
                    m.media.expected_record = &self.corrupt;
                    m.media.retained_record = &self.corrupt;
                }
                Stage::TruncatedRetained => m.media.retained_record = &self.corrupt[..229],
                Stage::EtagLocator => m.media.media = &self.etag_media,
                Stage::EmptyPages => m.pins.page_versions = &[],
                Stage::MismatchedMediaVersion => m.pins.media_version = "stale-version",
                Stage::NullPageVersion => m.pins.page_versions = &self.null_pages,
                Stage::Closed | Stage::Verified => {}
            }
            Some(m)
        }
        async fn lease_live(&self, g: &nntune_stream::grant::Grant) -> bool {
            self.inner.lease_live(g).await
        }
        async fn media(&self, g: S3Get<'_>) -> std::io::Result<S3GetReply<Self::Body>> {
            self.media_reads.set(self.media_reads.get() + 1);
            self.inner.media(g).await
        }
        async fn page(
            &self,
            b: &str,
            p: ReadbackPin<'_>,
            l: u32,
            u: SystemTime,
        ) -> std::io::Result<(String, Vec<u8>)> {
            self.inner.page(b, p, l, u).await
        }
    }

    let s = Fixture::from_plaintext(FLAC);
    let mut corrupt = s.envelope;
    corrupt[229] ^= 1;
    let loc = s.media.locator();
    let etag = S3Locator::new(
        loc.bucket().into(),
        loc.key().into(),
        S3Binding::Etag("\"e\"".into()),
    )
    .unwrap();
    let source = ReopenSource {
        inner: Source::new(Fault::None),
        wrong_root: [8; 32],
        corrupt,
        etag_media: S3MediaMetadata::from_trusted_context(etag, &s.context).unwrap(),
        null_pages: ["null".into()],
        stage: Cell::new(Stage::Closed),
        media_reads: Cell::new(0),
    };
    let key = b"test only, not a deployed secret";
    let req = |range: Option<&str>| PlayRouteRequest {
        track_id: "track".into(),
        token: Some(TOKEN.into()),
        range: range.map(str::to_owned),
        trace_id: None,
    };

    assert_eq!(
        serve::<ReopenSource, Writer<'_>>(&req(None), key, None, now, |_| panic!(), |_| panic!())
            .await
            .unwrap_err()
            .status,
        404
    );
    for (stage, status, expect_media) in [
        (Stage::Closed, 404, false),
        (Stage::WrongDigest, 404, false),
        (Stage::WrongRoot, 502, false),
        (Stage::CorruptEnvelope, 502, false),
        (Stage::TruncatedRetained, 502, false),
        (Stage::EtagLocator, 502, false),
        (Stage::EmptyPages, 502, false),
        (Stage::MismatchedMediaVersion, 502, false),
        (Stage::NullPageVersion, 502, true),
    ] {
        source.stage.set(stage);
        let reads = source.media_reads.get();
        let err = serve::<_, Writer<'_>>(
            &req(None),
            key,
            Some(&source),
            now,
            |_| panic!("{stage:?}"),
            |_| panic!("{stage:?}"),
        )
        .await
        .unwrap_err();
        assert_eq!(
            (
                err.status,
                source.media_reads.get(),
                source.inner.page_reads.get()
            ),
            (status, reads + usize::from(expect_media), 0),
            "{stage:?}"
        );
    }

    source.stage.set(Stage::Verified);
    for (range, want_status, want_range, slice) in [
        (None, 200, None, 0..FLAC.len()),
        (
            Some("bytes=-2"),
            206,
            Some("bytes 51-52/53"),
            FLAC.len() - 2..FLAC.len(),
        ),
    ] {
        let (output, aborted, mut report) = (RefCell::new(Vec::new()), Cell::new(false), None);
        serve(
            &req(range),
            key,
            Some(&source),
            now,
            |head| {
                assert_eq!(
                    (head.status, head.content_range.as_deref(), head.len),
                    (want_status, want_range, slice.len() as u64)
                );
                Ok(Some(Writer {
                    output: &output,
                    fault: Fault::None,
                    aborted: &aborted,
                    closed: None,
                    revoke_on_write: None,
                }))
            },
            |i| report = Some(i),
        )
        .await
        .unwrap();
        assert!(!aborted.get());
        assert_eq!(output.into_inner(), FLAC[slice.clone()]);
        assert_eq!(
            report.map(|r| r.start as usize..r.end as usize),
            Some(slice)
        );
    }
}
