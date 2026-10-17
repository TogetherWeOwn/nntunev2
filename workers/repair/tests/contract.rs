//! Contract tests for the repair Worker: request/manifest validation
//! and the idempotency plan, exercised without R2 or a Container.

use nntune_core::par2::scratch::object_key_for_file_name;
use nntune_repair::contract::{
    classify_container_reply, container_repair_body, in_key, manifest_key, out_key, parse_manifest,
    parse_request, plan, result_key, ContainerOutcome, Plan,
};

const JOB: &str = "job_0123456789abcdef";

fn request_json() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "job_id": JOB,
        "user_id": "user_01",
        "idempotency_key": format!("ingest:user_01:{}:upload", "a".repeat(64)),
        "par2_set_id": "par2-set-01",
        "missing_blocks": 3,
        "round": 1,
        "scratch_prefix": "ingest-scratch/job_0123456789abcdef/r1/",
    }))
    .unwrap()
}

fn manifest_json() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "files": [
            {"file_name": "track.flac", "key": object_key_for_file_name("track.flac"), "size": 7, "role": "data"},
            {"file_name": "set.par2", "key": object_key_for_file_name("set.par2"), "size": 8, "role": "par2"},
        ]
    }))
    .unwrap()
}

#[test]
fn canonical_request_parses_and_keys_derive_from_prefix() {
    let req = parse_request(&request_json()).unwrap();
    assert_eq!(req.job_id, JOB);
    assert_eq!(
        manifest_key(&req.scratch_prefix),
        "ingest-scratch/job_0123456789abcdef/r1/in/manifest.json"
    );
    assert_eq!(
        result_key(&req.scratch_prefix),
        "ingest-scratch/job_0123456789abcdef/r1/out/result.json"
    );
    let manifest = parse_manifest(&manifest_json()).unwrap();
    assert_eq!(
        in_key(&req.scratch_prefix, &manifest.files[0].key),
        format!(
            "ingest-scratch/job_0123456789abcdef/r1/in/{}",
            object_key_for_file_name("track.flac")
        )
    );
    assert!(out_key(&req.scratch_prefix, &manifest.files[0].key).contains("/out/"));
}

#[test]
fn request_with_forged_prefix_is_rejected() {
    // Prefix must be exactly the canonical one for (job_id, round): a
    // caller cannot aim repair at another job's staged objects.
    let mut value: serde_json::Value = serde_json::from_slice(&request_json()).unwrap();
    value["scratch_prefix"] = serde_json::json!("ingest-scratch/other_job/r1/");
    let err = parse_request(&serde_json::to_vec(&value).unwrap()).unwrap_err();
    assert_eq!((err.status, err.code), (400, "invalid_request"));

    value["scratch_prefix"] = serde_json::json!("ingest-scratch/job_0123456789abcdef/r2/");
    assert!(parse_request(&serde_json::to_vec(&value).unwrap()).is_err());

    value["scratch_prefix"] = serde_json::json!("ingest-scratch/job_0123456789abcdef/r1/");
    value["round"] = serde_json::json!(4);
    assert!(parse_request(&serde_json::to_vec(&value).unwrap()).is_err());

    let mut value: serde_json::Value = serde_json::from_slice(&request_json()).unwrap();
    value["missing_blocks"] = serde_json::json!(0);
    assert!(parse_request(&serde_json::to_vec(&value).unwrap()).is_err());
}

#[test]
fn manifest_validation_mirrors_the_container_rules() {
    assert!(parse_manifest(&manifest_json()).is_ok());

    // Key that is not sha256(file_name).
    let mut value: serde_json::Value = serde_json::from_slice(&manifest_json()).unwrap();
    value["files"][0]["key"] = serde_json::json!("b".repeat(64));
    assert!(parse_manifest(&serde_json::to_vec(&value).unwrap()).is_err());

    // Traversal file name.
    let mut value: serde_json::Value = serde_json::from_slice(&manifest_json()).unwrap();
    value["files"][0]["file_name"] = serde_json::json!("../track.flac");
    value["files"][0]["key"] = serde_json::json!(object_key_for_file_name("../track.flac"));
    assert!(parse_manifest(&serde_json::to_vec(&value).unwrap()).is_err());

    // No par2 file.
    let mut value: serde_json::Value = serde_json::from_slice(&manifest_json()).unwrap();
    value["files"][1]["role"] = serde_json::json!("data");
    assert!(parse_manifest(&serde_json::to_vec(&value).unwrap()).is_err());

    // Duplicate file names.
    let mut value: serde_json::Value = serde_json::from_slice(&manifest_json()).unwrap();
    value["files"][1]["file_name"] = serde_json::json!("track.flac");
    value["files"][1]["key"] = serde_json::json!(object_key_for_file_name("track.flac"));
    assert!(parse_manifest(&serde_json::to_vec(&value).unwrap()).is_err());
}

#[test]
fn existing_result_short_circuits_the_paid_run() {
    let req = parse_request(&request_json()).unwrap();
    assert_eq!(plan(&req, None).unwrap(), Plan::Run);

    let stored = br#"{"status":"repaired","exit_code":0,"files":[]}"#;
    match plan(&req, Some(stored)).unwrap() {
        Plan::AlreadyDone(result) => assert_eq!(result.status.as_str(), "repaired"),
        Plan::Run => panic!("stored result must short-circuit"),
    }

    // A corrupt stored result is a server fault, never a silent re-run.
    let err = plan(&req, Some(b"not json")).unwrap_err();
    assert_eq!(err.status, 500);
}

#[test]
fn container_body_carries_only_the_manifest() {
    let req = parse_request(&request_json()).unwrap();
    let manifest = parse_manifest(&manifest_json()).unwrap();
    let body = container_repair_body(&req, &manifest);
    let value = serde_json::to_value(&body).unwrap();
    assert_eq!(value["job_id"], JOB);
    assert_eq!(value["round"], 1);
    assert_eq!(value["files"].as_array().unwrap().len(), 2);
    // No user ids, idempotency keys or credentials reach the Container.
    assert!(value.get("user_id").is_none());
    assert!(value.get("idempotency_key").is_none());
}

#[test]
fn container_replies_classify_for_the_ingest_mapping() {
    match classify_container_reply(200, br#"{"status":"repaired","exit_code":0,"files":[]}"#) {
        ContainerOutcome::Done(result) => assert_eq!(result.status.as_str(), "repaired"),
        other => panic!("expected Done, got {other:?}"),
    }
    match classify_container_reply(503, br#"{"error":"repair_timeout"}"#) {
        ContainerOutcome::Forward(status, code) => {
            assert_eq!((status, code.as_str()), (503, "repair_timeout"))
        }
        other => panic!("expected Forward, got {other:?}"),
    }
    match classify_container_reply(409, br#"{"error":"repair_manifest_conflict"}"#) {
        ContainerOutcome::Forward(status, code) => {
            assert_eq!((status, code.as_str()), (409, "repair_manifest_conflict"))
        }
        other => panic!("expected Forward, got {other:?}"),
    }
    // Unknown or malformed replies become a retryable container fault.
    match classify_container_reply(200, b"garbage") {
        ContainerOutcome::Forward(status, code) => {
            assert_eq!((status, code.as_str()), (502, "container_unavailable"))
        }
        other => panic!("expected Forward, got {other:?}"),
    }
    match classify_container_reply(502, b"") {
        ContainerOutcome::Forward(status, _) => assert_eq!(status, 502),
        other => panic!("expected Forward, got {other:?}"),
    }
}
