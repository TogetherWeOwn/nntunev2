//! Stored result marker failures close server-side: an oversize or
//! corrupt `out/result.json` is a 500 (never a silent rerun of a
//! possibly-paid encode), while an exactly-at-cap valid marker
//! replays.

use nntune_transcode::contract::{parse_request, plan, ApiFault, Plan, MAX_RESULT_BYTES};

const JOB: &str = "job_0123456789abcdef";
const HEX64: &str = "9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734";

fn request() -> nntune_transcode::contract::TranscodeRequest {
    parse_request(
        &serde_json::to_vec(&serde_json::json!({
            "job_id": JOB,
            "user_id": "user_01",
            "idempotency_key": format!("ingest:user_01:{}:upload", "a".repeat(64)),
            "input_key": format!("0007-{HEX64}"),
            "source_sha256": HEX64,
            "scratch_prefix": format!("ingest-scratch/{JOB}/r1/"),
            "staging_id": HEX64,
        }))
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn oversize_and_corrupt_markers_are_server_faults() {
    let req = request();
    assert_eq!(
        plan(&req, Some(&vec![b'x'; MAX_RESULT_BYTES + 1]), HEX64),
        Err(ApiFault::new(500, "transcode_unavailable"))
    );
    assert_eq!(
        plan(&req, Some(b"{truncated"), HEX64),
        Err(ApiFault::new(500, "transcode_unavailable"))
    );
    // Valid JSON of the wrong shape is equally unplannable.
    assert_eq!(
        plan(&req, Some(br#"{"status":42}"#), HEX64),
        Err(ApiFault::new(500, "transcode_unavailable"))
    );
}

#[test]
fn exact_cap_valid_marker_replays() {
    let req = request();
    // Unknown fields are ignored, which isolates the byte ceiling
    // from marker validation.
    let mut value = serde_json::json!({
        "status": "transcoded",
        "exit_code": 0,
        "artifact_sha256": HEX64,
        "byte_size": 58,
        "staging_id": HEX64,
    });
    let base = serde_json::to_vec(&value).unwrap();
    value["padding"] = "x".repeat(MAX_RESULT_BYTES - base.len() - 14).into();
    let mut body = serde_json::to_vec(&value).unwrap();
    while body.len() < MAX_RESULT_BYTES {
        value["padding"] = format!("{}x", value["padding"].as_str().unwrap()).into();
        body = serde_json::to_vec(&value).unwrap();
    }
    assert_eq!(body.len(), MAX_RESULT_BYTES);
    assert!(matches!(
        plan(&req, Some(&body), HEX64),
        Ok(Plan::AlreadyDone(_))
    ));
}

#[test]
fn empty_current_staging_never_replays() {
    let req = request();
    let stored = serde_json::to_vec(&serde_json::json!({
        "status": "transcoded",
        "exit_code": 0,
        "artifact_sha256": HEX64,
        "byte_size": 58,
        "staging_id": HEX64,
    }))
    .unwrap();
    // Defensive: parsed requests always carry a staging id, so an
    // empty current id means the caller bypassed validation.
    assert_eq!(plan(&req, Some(&stored), ""), Ok(Plan::Rerun));
}
