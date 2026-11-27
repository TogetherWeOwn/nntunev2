use std::future::pending;

use nntune_core::{
    nzb::{File, Segment},
    original_source::OriginalSource,
};
use nntune_stream::{
    api_error::ApiError,
    cache::ContentCache,
    grant,
    meter::MemoryMeter,
    original_play::{authorize_original, control::OriginalRequestControl},
    play::PlayError,
    play_route::{
        bearer_token, serve_play, MemoryManifestStore, PlayRouteOutcome, PlayRouteRefusal,
        PlayRouteRequest,
    },
    pool::Pool,
    routes::{consume_body_bytes, decide_public_route, PublicRoute},
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
fn granted_cache_range_stays_private() {
    let mut cache = ContentCache::new();
    cache.put_track(TRACK, vec![0, 1, 2, 3]);
    let result = serve_play(
        &mut Pool::new(1),
        &mut cache,
        &mut MemoryMeter::default(),
        SECRET,
        &MemoryManifestStore::default(),
        "127.0.0.1:0".parse().unwrap(),
        NOW,
        &request(),
    );
    let PlayRouteOutcome::Serve(response) = result else {
        panic!("bound range refused")
    };
    assert_eq!((response.status, response.body_len), (206, 2));
    for (name, value) in [
        ("content-range", "bytes 1-2/4"),
        ("content-length", "2"),
        ("cache-control", "private, no-store"),
    ] {
        assert!(response.headers.contains(&(name.into(), value.into())));
    }
    assert_eq!(response.play.flatten().collect::<Vec<_>>(), [1, 2]);
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
    let request = request();
    for (now, status, code) in [
        (NOW, 502, "upstream_unavailable"),
        (EXPIRY, 401, "unauthorized"),
    ] {
        let mut control = OriginalRequestControl::new(EXPIRY, pending(), || now);
        control.cancellation().cancel();
        let error = authorize_original(&request, SECRET, NOW)
            .unwrap()
            .prepare_owned(source.clone(), Some("bytes=4-"), &mut control)
            .err()
            .expect("the plan must be refused");
        assert_eq!((error.status, error.api_error.code), (status, code));
        assert_eq!(error.content_range, None);
    }
}

#[test]
fn body_budget_refuses_overrun_and_short_eof() {
    let mut remaining = 4;
    assert!(consume_body_bytes(&mut remaining, Some(3)));
    assert!(!consume_body_bytes(&mut remaining, Some(2)));
    assert!(!consume_body_bytes(&mut remaining, None));
    assert!(consume_body_bytes(&mut remaining, Some(1)));
    assert!(consume_body_bytes(&mut remaining, None));
    assert!(!consume_body_bytes(&mut remaining, Some(1)));
}

#[test]
fn error_codes_use_public_messages_and_registry_status() {
    let invalid = ApiError::by_code("invalid_request", "private provider detail");
    assert_eq!(invalid, ApiError::invalid_request());
    let failure = PlayError::PoolQuarantined {
        retry_after_ms: 1001,
    };
    let refusal = PlayRouteRefusal::from(&failure);
    assert_eq!(
        (
            failure.http_status(),
            refusal.status,
            refusal.api_error.status
        ),
        (503, 503, 503)
    );
    assert_eq!(refusal.retry_after.as_deref(), Some("2"));
}
