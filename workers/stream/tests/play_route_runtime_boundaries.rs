use std::{
    cell::Cell,
    future::{pending, ready},
    task::Poll,
};

use futures_util::FutureExt;
use nntune_core::{
    nzb::{File, Segment},
    original_source::OriginalSource,
};
use nntune_stream::{
    api_error::ApiError,
    grant,
    original_play::{authorize_original, control::OriginalRequestControl},
    play::PlayError,
    play_route::{bearer_token, MeteredBody, PlayRouteRefusal, PlayRouteRequest},
    routes::{decide_public_route, dial_before_deadline, PublicRoute},
};

const TRACK: &str = "00000000-0000-0000-0000-000000000001";
const NOW: u64 = 1000;
const EXPIRY: u64 = NOW + 60;
const SECRET: &[u8] = b"fixture signing material";

fn request() -> PlayRouteRequest {
    PlayRouteRequest {
        track_id: TRACK.into(),
        token: Some(grant::issue_v2(
            "fixture-user",
            TRACK,
            "fixture-play",
            1,
            EXPIRY,
            SECRET,
        )),
        range: Some("bytes=1-2".into()),
        trace_id: None,
    }
}

#[test]
fn public_dispatch_and_bearer_reject_ambiguous_inputs() {
    assert_eq!(
        decide_public_route("PROPFIND", "/play/track"),
        PublicRoute::NotFound
    );
    for malformed in ["Bearer v2.a.b\t", "Bearer v2.a.b,Other", "Bearer v2.a.é"] {
        assert_eq!(bearer_token(Some(malformed)), None);
    }
    assert_eq!(bearer_token(Some("bEaReR v2.A-_.b")), Some("v2.A-_.b"));
}

#[test]
fn original_control_failure_and_grant_expiry_keep_distinct_statuses() {
    let file = File {
        ordinal: 0,
        subject: String::new(),
        filename: None,
        role: "content".into(),
        container: "flac".into(),
        posted_at_unix: 0,
        segments: vec![Segment {
            ordinal: 0,
            number: 1,
            message_id: "part@fixture.invalid".into(),
            encoded_bytes: 512,
        }],
    };
    let source = OriginalSource::from_file(TRACK, "fixture-job", &file, 4, None).unwrap();
    for (now, status, code) in [
        (NOW, 502, "upstream_unavailable"),
        (EXPIRY, 401, "unauthorized"),
    ] {
        let mut control = OriginalRequestControl::new(EXPIRY, pending(), || now);
        control.cancellation().cancel();
        let error = authorize_original(&request(), SECRET, NOW)
            .unwrap()
            .prepare_owned(source.clone(), Some("bytes=4-"), &mut control)
            .err()
            .expect("the plan must be refused");
        assert_eq!((error.status, error.api_error.code), (status, code));
        assert_eq!(error.content_range, None);
    }
}

#[test]
fn metered_body_rejects_overrun_and_short_eof() {
    for (chunks, accepted, terminal, reported) in [
        (&[5][..], 0, Some(Err("length")), None),
        (&[2, 3, 1][..], 1, Some(Err("length")), Some(2)),
        (&[2][..], 1, Some(Err("length")), Some(2)),
        (&[2, 2][..], 2, None, Some(4)),
    ] {
        let delivered = Cell::new(None);
        let reader = chunks.iter().map(|&len| Ok(vec![0; len]));
        let mut body = MeteredBody::new(reader, 4, |bytes| delivered.set(Some(bytes)));
        let mut pull = || body.poll_chunk(|reader| Poll::Ready(reader.next()), || "length");
        for &len in &chunks[..accepted] {
            assert_eq!(pull(), Poll::Ready(Some(Ok(vec![0; len]))));
        }
        assert_eq!(pull(), Poll::Ready(terminal));
        assert_eq!(
            body.poll_chunk(|_| panic!("read after termination"), || "length"),
            Poll::Ready(None)
        );
        drop(body);
        assert_eq!(delivered.get(), reported);
    }
}

#[test]
fn handshake_deadline_stops_a_pending_dial() {
    let refused = dial_before_deadline(ready(()), pending::<nntune_core::Result<()>>());
    assert!(matches!(
        refused.now_or_never(),
        Some(Err(nntune_core::Error::Io(error))) if error.kind() == std::io::ErrorKind::TimedOut
    ));
}

#[test]
fn error_codes_use_public_messages_and_registry_status() {
    let invalid = ApiError::by_code("invalid_request", "private provider detail");
    assert_eq!(invalid, ApiError::invalid_request());
    let failure = PlayError::PoolQuarantined {
        retry_after_ms: 1001,
    };
    let refusal = PlayRouteRefusal::from(&failure);
    assert_eq!(failure.http_status(), 503);
    assert_eq!(refusal.status, refusal.api_error.status);
    assert_eq!(refusal.retry_after.as_deref(), Some("2"));
}
