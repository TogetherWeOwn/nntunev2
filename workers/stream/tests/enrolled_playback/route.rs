use super::*;
use nntune_stream::{enrolled_playback::route::serve, play_route::PlayRouteRequest};
use std::cell::{Cell, RefCell};

#[path = "../fixtures/private_enrolled_source.rs"]
mod fixture;
use fixture::{Fault, Source, Writer, FLAC};

const TOKEN: &str = include_str!("../../../../fixtures/play-grant/private-enrolled.token");
// Pre-signed synthetic v4 grant for Fixture::new(), expiring at Unix 12.
const BROWSER_TOKEN: &str = concat!(
    "v4.bWVtYmVyfHRyYWNrfHByaXZhdGVfYnJvd3Nlcl9BQUFBQUFBQUFBQUFBQUFBQUFBQUFBfDQyfDEyf",
    "GNmMjQ0ZjZkYzYxYWI3ZTU3MTI4MzYwODM2NGZmZGViZTAzZjBiNGZmMGE2MDcyNDE5MDFmOTUzZWE5MDY2ZGE.",
    "5OZv4_yGbRpUk6HYDXTckBYY046ImHeyN71sQOoZ0MA",
);

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
