//! Contract tests for the transcode Worker: request validation,
//! key derivation, the staging-id idempotency plan, the ingest reply
//! shape, and Container reply classification — exercised without R2
//! or a Container.

use nntune_transcode::contract::{
    artifact_key, classify_container_reply, container_transcode_body, in_key, ingest_reply_body,
    parse_request, plan, result_key, ApiFault, ContainerOutcome, Plan, TranscodeResult,
    TranscodeStatus,
};

const JOB: &str = "job_0123456789abcdef";
const HEX64: &str = "9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734";
const PREFIX: &str = "ingest-scratch/job_0123456789abcdef/r1/";

fn request_json() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "job_id": JOB,
        "user_id": "user_01",
        "idempotency_key": format!("ingest:user_01:{}:upload", "a".repeat(64)),
        "input_key": format!("0007-{HEX64}"),
        "source_sha256": HEX64,
        "scratch_prefix": PREFIX,
        "staging_id": HEX64,
    }))
    .unwrap()
}

fn stored_result(staging_id: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "status": "transcoded",
        "exit_code": 0,
        "artifact_sha256": HEX64,
        "byte_size": 58,
        "staging_id": staging_id,
    }))
    .unwrap()
}

#[test]
fn well_formed_request_parses() {
    let req = parse_request(&request_json()).unwrap();
    assert_eq!(req.job_id, JOB);
    assert_eq!(req.input_key, format!("0007-{HEX64}"));
    assert_eq!(req.scratch_prefix, PREFIX);
    assert_eq!(req.staging_id, HEX64);
}

#[test]
fn each_bad_field_rejects() {
    let base: serde_json::Value = serde_json::from_slice(&request_json()).unwrap();
    let mutate = |field: &str, value: serde_json::Value| {
        let mut body = base.clone();
        body[field] = value;
        serde_json::to_vec(&body).unwrap()
    };
    // job alphabet, user shape, key shape, input-key shape, hash,
    // prefix containment, staging alphabet.
    assert!(parse_request(&mutate("job_id", "bad job!".into())).is_err());
    assert!(parse_request(&mutate("user_id", "bad:user".into())).is_err());
    assert!(parse_request(&mutate("idempotency_key", "nope".into())).is_err());
    assert!(parse_request(&mutate("input_key", HEX64.into())).is_err());
    assert!(parse_request(&mutate("input_key", "ZZZ".into())).is_err());
    assert!(parse_request(&mutate("source_sha256", "short".into())).is_err());
    assert!(parse_request(&mutate("scratch_prefix", "ingest-scratch/other/r1/".into())).is_err());
    assert!(parse_request(&mutate("staging_id", "short".into())).is_err());
    // Missing fields and malformed JSON reject.
    let mut missing = base.clone();
    missing.as_object_mut().unwrap().remove("staging_id");
    assert!(parse_request(&serde_json::to_vec(&missing).unwrap()).is_err());
    assert!(parse_request(b"not json").is_err());
}

#[test]
fn uppercase_source_hash_parses() {
    let mut body: serde_json::Value = serde_json::from_slice(&request_json()).unwrap();
    body["source_sha256"] = HEX64.to_ascii_uppercase().into();
    let req = parse_request(&serde_json::to_vec(&body).unwrap()).unwrap();
    assert_eq!(req.source_sha256, HEX64.to_ascii_uppercase());
}

#[test]
fn scratch_keys_derive_from_prefix() {
    assert_eq!(result_key(PREFIX), format!("{PREFIX}out/result.json"));
    assert_eq!(in_key(PREFIX, "0007-abc"), format!("{PREFIX}in/0007-abc"));
    assert_eq!(artifact_key(PREFIX), format!("{PREFIX}out/artifact.ogg"));
}

#[test]
fn plan_keys_idempotency_on_staging() {
    let req = parse_request(&request_json()).unwrap();
    assert_eq!(plan(&req, None, HEX64), Ok(Plan::Run));
    // Same staging replays.
    let done = plan(&req, Some(&stored_result(HEX64)), HEX64).unwrap();
    assert!(matches!(done, Plan::AlreadyDone(_)));
    if let Plan::AlreadyDone(result) = done {
        assert_eq!(result.status, TranscodeStatus::Transcoded);
        assert_eq!(result.artifact_sha256.as_deref(), Some(HEX64));
    }
    // Another staging reruns — the previous input's verdict never replays.
    assert_eq!(
        plan(
            &req,
            Some(&stored_result(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            )),
            HEX64
        ),
        Ok(Plan::Rerun)
    );
    // Unstamped (legacy) results rerun.
    let unstamped = serde_json::to_vec(&serde_json::json!({
        "status": "transcoded",
        "exit_code": 0,
        "artifact_sha256": HEX64,
        "byte_size": 58,
    }))
    .unwrap();
    assert_eq!(plan(&req, Some(&unstamped), HEX64), Ok(Plan::Rerun));
    // Corrupt or oversize markers are server faults, never silent reruns.
    assert_eq!(
        plan(&req, Some(b"not json"), HEX64),
        Err(ApiFault::new(500, "transcode_unavailable"))
    );
    assert_eq!(
        plan(&req, Some(&vec![b'x'; (1 << 20) + 1]), HEX64),
        Err(ApiFault::new(500, "transcode_unavailable"))
    );
}

#[test]
fn reply_shape_matches_the_ingest_classifier_reads() {
    // Ingest reads `status == "transcoded"` plus `artifact_sha256` and
    // `byte_size`; `status == "untranscodable"` alone is terminal.
    let transcoded = TranscodeResult {
        status: TranscodeStatus::Transcoded,
        exit_code: Some(0),
        artifact_sha256: Some(HEX64.to_string()),
        byte_size: Some(58),
        staging_id: HEX64.to_string(),
    };
    assert_eq!(
        ingest_reply_body(&transcoded),
        format!(r#"{{"status":"transcoded","artifact_sha256":"{HEX64}","byte_size":58}}"#)
            .as_bytes()
    );
    let untranscodable = TranscodeResult {
        status: TranscodeStatus::Untranscodable,
        exit_code: Some(1),
        artifact_sha256: None,
        byte_size: None,
        staging_id: HEX64.to_string(),
    };
    assert_eq!(
        ingest_reply_body(&untranscodable),
        br#"{"status":"untranscodable"}"#
    );
}

#[test]
fn container_body_carries_input_identity_only() {
    let req = parse_request(&request_json()).unwrap();
    let body = container_transcode_body(&req);
    assert_eq!(body.job_id, JOB);
    assert_eq!(body.staging_id, HEX64);
    assert_eq!(body.input_key, format!("0007-{HEX64}"));
    assert_eq!(body.source_sha256, HEX64);
    // No user identity, idempotency keys or credentials cross into the
    // Container — the serialized body must not name them.
    let json = serde_json::to_string(&body).unwrap();
    assert!(!json.contains("user_id"));
    assert!(!json.contains("idempotency"));
    assert_eq!(
        json,
        format!(
            r#"{{"job_id":"{JOB}","staging_id":"{HEX64}","input_key":"0007-{HEX64}","source_sha256":"{HEX64}"}}"#
        )
    );
}

#[test]
fn container_reply_classification_is_terminal_or_forward() {
    let done = serde_json::to_vec(&serde_json::json!({
        "status": "transcoded",
        "exit_code": 0,
        "artifact_sha256": HEX64,
        "byte_size": 58,
    }))
    .unwrap();
    assert!(matches!(
        classify_container_reply(200, &done),
        ContainerOutcome::Done(_)
    ));
    // Malformed or oversized 200s are never terminal.
    assert_eq!(
        classify_container_reply(200, b"not json"),
        ContainerOutcome::Forward(502, "container_unavailable".to_string())
    );
    assert_eq!(
        classify_container_reply(200, &vec![b'x'; (1 << 20) + 1]),
        ContainerOutcome::Forward(502, "container_unavailable".to_string())
    );
    // Known codes forward with their status; anything else 502s.
    for status in [400, 404, 409, 413, 503] {
        assert_eq!(
            classify_container_reply(status, br#"{"error":"transcode_timeout"}"#),
            ContainerOutcome::Forward(status, "transcode_timeout".to_string())
        );
    }
    assert_eq!(
        classify_container_reply(500, br#"{"error":"x"}"#),
        ContainerOutcome::Forward(502, "container_unavailable".to_string())
    );
    // Unshaped codes never forward verbatim.
    assert_eq!(
        classify_container_reply(400, br#"{"error":"Bad Code!"}"#),
        ContainerOutcome::Forward(502, "container_unavailable".to_string())
    );
    assert_eq!(
        classify_container_reply(400, b"not json"),
        ContainerOutcome::Forward(502, "container_unavailable".to_string())
    );
}
