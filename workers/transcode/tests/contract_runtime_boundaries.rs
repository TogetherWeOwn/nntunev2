use std::future::{pending, Future};
use std::task::{Context, Poll};

use futures_util::{stream, FutureExt, StreamExt};
use nntune_transcode::contract::{
    classify_container_reply, parse_request, plan, read_job_request, transcode_unavailable_fault,
    validate_do_reply, ApiFault, ContainerOutcome, MAX_REQUEST_BYTES, MAX_RESULT_BYTES,
};
use serde_json::{json, Value};

const RUNTIME: &str = include_str!("../src/runtime.rs");

fn job() -> Value {
    let sha = "9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734";
    json!({
        "job_id": "job_01J9ZQ8K2M",
        "user_id": "user_01",
        "idempotency_key": format!("ingest:user_01:{sha}:upload"),
        "input_key": format!("0007-{sha}"),
        "scratch_prefix": "ingest-scratch/job_01J9ZQ8K2M/r1/",
        "source_sha256": sha.to_ascii_uppercase(),
        "staging_id": "64e7ca9681a8c87e6d2acc5629774dcdfd27387c9b64b71d81273e075f923a9e",
    })
}

#[test]
fn staging_identity_and_encoder_parameters_are_checked_before_dispatch() {
    let entrypoints = RUNTIME.matches("match read_request(&mut req).await");
    assert_eq!(entrypoints.count(), 2);
    assert!(RUNTIME.contains("read_job_request(stream.map"));
    let read = |body: Value| {
        read_job_request(stream::iter([Ok(body.to_string().into_bytes())]), pending())
            .now_or_never()
            .unwrap()
    };
    assert!(read(job()).is_ok());
    for (field, value) in [
        ("codec", json!("mp3")),
        ("bitrate", json!(320_000)),
        ("timeout_secs", json!(600)),
        ("scratch_prefix", json!("ingest-scratch/job_01J9ZQ8K2M/r2/")),
        ("source_sha256", json!("a".repeat(64))),
        ("input_key", json!(format!("0008-{}", "a".repeat(64)))),
    ] {
        let mut body = job();
        body[field] = value;
        assert_eq!(read(body), Err(ApiFault::invalid()), "{field}");
    }
}

#[test]
fn request_cap_and_deadline_apply_until_eof() {
    assert!(RUNTIME.contains("select(deadline, Box::pin(self.run(&parsed)))"));
    let mut bytes = job().to_string().into_bytes();
    bytes.resize(MAX_REQUEST_BYTES, b' ');
    let read = |tail| {
        read_job_request(stream::iter([Ok(bytes.clone()), Ok(tail)]), pending())
            .now_or_never()
            .unwrap()
    };
    assert!(read(vec![]).is_ok());
    assert_eq!(read(vec![b' ']).unwrap_err().status, 413);
    let mut cx = Context::from_waker(futures_util::task::noop_waker_ref());
    let (deadline, fire) = futures_util::future::abortable(pending::<()>());
    let stalled = stream::iter([Ok(bytes)]).chain(stream::pending());
    let mut read = Box::pin(read_job_request(stalled, deadline.map(|_| ())));
    assert!(read.as_mut().poll(&mut cx).is_pending());
    fire.abort();
    let expired = Poll::Ready(Err(transcode_unavailable_fault()));
    assert_eq!(read.as_mut().poll(&mut cx), expired);
}

#[test]
fn corrupt_result_markers_fail_before_replay_or_rerun() {
    let req = parse_request(job().to_string().as_bytes()).unwrap();
    let fault = ApiFault::new(500, "transcode_unavailable");
    for marker in [
        json!({"status":"transcoded", "staging_id":req.staging_id}),
        json!({"status":"untranscodable", "staging_id":req.staging_id, "artifact_sha256":"private_diagnostic"}),
        json!({"status":"untranscodable", "staging_id":"invalid"}),
    ] {
        let marker = marker.to_string();
        let result = plan(&req, Some(marker.as_bytes()), &req.staging_id);
        assert_eq!(result.unwrap_err(), fault);
    }
}

#[test]
fn reply_boundaries_accept_only_bounded_public_envelopes() {
    assert!(RUNTIME.contains("validate_do_reply(status, &bytes)?"));
    let timeout = br#"{"error":"transcode_timeout"}"#;
    let mut body = timeout.to_vec();
    body.resize(MAX_RESULT_BYTES, b' ');
    let forwarded = ContainerOutcome::Forward(503, "transcode_timeout".into());
    assert_eq!(classify_container_reply(503, &body), forwarded);
    body.push(b' ');
    let rejected = ContainerOutcome::Forward(502, "container_unavailable".into());
    for body in [body.as_slice(), br#"{"error":"private_diagnostic"}"#] {
        assert_eq!(classify_container_reply(503, body), rejected);
    }
    assert_eq!(validate_do_reply(503, timeout), Ok(()));
    assert!(validate_do_reply(200, br#"{"status":"untranscodable"}"#).is_ok());
    let rejected = Err(transcode_unavailable_fault());
    assert_eq!(validate_do_reply(503, &body[..MAX_RESULT_BYTES]), rejected);
    for (status, body) in [
        (400, timeout.as_slice()),
        (404, br#"{"error":"not_found","detail":"private"}"#),
        (502, br#"["container_unavailable"]"#),
        (200, br#"{"status":"untranscodable","byte_size":1}"#),
    ] {
        assert_eq!(validate_do_reply(status, body), rejected);
    }
}
