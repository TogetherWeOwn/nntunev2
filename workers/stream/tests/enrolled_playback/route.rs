use super::*;
use nntune_stream::{enrolled_playback::route::serve, play_route::PlayRouteRequest};
use std::cell::RefCell;

#[path = "../fixtures/private_enrolled_source.rs"]
mod fixture;
use fixture::{Fault, Source, Writer};

const FLAC: &[u8] =
    include_bytes!("../../../../fixtures/synthetic-flac/admin-upload/fixture-1.flac");
const TOKEN: &str = include_str!("../../../../fixtures/play-grant/private-enrolled.token");

#[tokio::test]
async fn private_route_plays_and_seeks_encrypted_flac_or_refuses_without_delivery() {
    let fixture = Fixture::from_plaintext(FLAC);
    let sealed = [&fixture.context.header[..], &fixture.first].concat();
    let mut source = Source {
        fixture,
        sealed,
        fault: Fault::None,
    };
    let key = b"test only, not a deployed secret";
    assert!(nntune_stream::grant::verify(TOKEN, key, 10).is_err());
    let calls = [
        (Fault::None, None, true, 200, 0..FLAC.len()),
        (
            Fault::None,
            Some("bytes=-2"),
            true,
            206,
            FLAC.len() - 2..FLAC.len(),
        ),
        (Fault::Denied, None, true, 404, 0..0),
        (Fault::LeaseLost, None, true, 404, 0..0),
        (Fault::None, None, false, 401, 0..0),
        (Fault::MediaVersion, None, true, 502, 0..0),
        (Fault::PageVersion, None, true, 502, 0..0),
        (Fault::Ciphertext, None, true, 502, 0..0),
    ];
    for (fault, range, live, expected, slice) in calls {
        source.fault = fault;
        if fault == Fault::Ciphertext {
            source.sealed[48] ^= 1;
        }
        let req = PlayRouteRequest {
            track_id: "track".into(),
            token: Some(TOKEN.into()),
            range: range.map(str::to_owned),
            trace_id: None,
        };
        let output = RefCell::new(Vec::new());
        let mut status = 0;
        let mut report = None;
        let outcome = serve(
            &req,
            key,
            Some(&source),
            || now() + Duration::from_secs(if live { 0 } else { 2 }),
            |head| {
                status = head.status;
                assert_eq!(
                    head.len,
                    if expected < 300 {
                        slice.len() as u64
                    } else {
                        FLAC.len() as u64
                    }
                );
                if expected == 206 {
                    assert_eq!(head.content_range.as_deref(), Some("bytes 51-52/53"));
                }
                Ok(Some(Writer(&output)))
            },
            |interval| report = Some(interval),
        )
        .await;
        assert_eq!(
            outcome.map_or_else(|error| error.status, |()| status),
            expected
        );
        assert_eq!(output.into_inner(), FLAC[slice.clone()]);
        assert_eq!(
            report.map(|r| r.start as usize..r.end as usize),
            (!slice.is_empty()).then_some(slice)
        );
    }
}
