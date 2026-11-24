use super::*;
use nntune_stream::{enrolled_playback::route::serve, play_route::PlayRouteRequest};
use std::cell::RefCell;

#[path = "../fixtures/private_enrolled_source.rs"]
mod fixture;
use fixture::{Fault, Source, Writer, FLAC};

const TOKEN: &str = include_str!("../../../../fixtures/play-grant/private-enrolled.token");

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
                Ok((status != 416).then_some(Writer(&output, case.fault)))
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
        let slice = case.delivered;
        assert_eq!(output.into_inner(), FLAC[slice.clone()]);
        assert_eq!(
            report.map(|r| r.start as usize..r.end as usize),
            (!slice.is_empty()).then_some(slice)
        );
    }
}
