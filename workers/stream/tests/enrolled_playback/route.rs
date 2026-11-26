use super::*;
use nntune_stream::{enrolled_playback::route::serve, play_route::PlayRouteRequest};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    task::Poll,
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
    "v4.bWVtYmVyfG90aGVyLXRyYWNrfHByaXZhdGVfYnJvd3Nlcl9CQkJCQkJCQkJCQkJCQkJCQkJCQkJCfDQyfDEy",
    "fDU3ODZjZjBjNjhlNTBiZjg1MjU3NTk2NTU3ZTJhZjZkN2Q2YWNkZWFiZDRlYTJiYjNlMDkzNjQxNzRjMmIwZTc.",
    "5IZubhMIL7iiCcfEddhINzeSlevOTCFChl9AJskiNuo",
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
    let interrupted_source = Source::browser();
    let complete_source = Source::other_track();
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
    let start_read = |req, source, output, aborted| {
        serve(
            req,
            b"test only, not a deployed secret",
            Some(source),
            now,
            |head| {
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
                    revoke_on_write: None,
                }))
            },
            |receipt| {
                assert!(meter.borrow_mut().record(&receipt, 10));
                reports.borrow_mut().push(receipt);
            },
        )
    };
    let mut interrupted = Box::pin(start_read(
        &interrupted_request,
        &interrupted_source,
        &interrupted_output,
        &interrupted_aborted,
    ));
    let mut complete = Box::pin(start_read(
        &complete_request,
        &complete_source,
        &complete_output,
        &complete_aborted,
    ));
    assert!(futures_util::poll!(&mut interrupted).is_pending());
    assert!(futures_util::poll!(&mut complete).is_pending());
    assert!(interrupted_gate.waiting.get());
    assert!(complete_gate.waiting.get());
    assert_eq!(*interrupted_output.borrow(), b"AY");
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
        assert_eq!(status, case.response_status());
        assert_eq!(
            aborted.get(),
            matches!(case.fault, Fault::Writer | Fault::ExpiredAtWrite)
        );
        let slice = case.delivered;
        assert_eq!(output.into_inner(), FLAC[slice.clone()]);
        assert_eq!(
            report.map(|r| r.start as usize..r.end as usize),
            (!slice.is_empty()).then_some(slice)
        );
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
