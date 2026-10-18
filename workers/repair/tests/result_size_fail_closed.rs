//! Pin stored-result size fail-closed read-back.
//!
//! [`plan`] checks `existing_result.len() > [`MAX_RESULT_BYTES`]` before
//! JSON parse: a `MAX + 1`-byte body is 500 `repair_unavailable` even when
//! the first `MAX` bytes would otherwise parse (trailing-whitespace padding
//! keeps the padded fixture valid JSON, so only the size gate rejects it),
//! corrupt under-cap bytes are the same 500, an under-cap valid result with
//! a matching digest is [`Plan::AlreadyDone`], a valid result padded to
//! exactly `MAX` is also [`Plan::AlreadyDone`] (the gate is `>`, pinning the
//! boundary against a `>=` regression), and exactly-`MAX` invalid JSON still
//! 500s via the parser.

use nntune_core::par2::scratch::object_key_for_file_name;
use nntune_repair::contract::{
    manifest_digest, plan, FileRole, OutputFile, Plan, RepairRequest, RepairResult, RepairStatus,
    ScratchFile, ScratchManifest, MAX_RESULT_BYTES,
};

const JOB: &str = "job_0123456789abcdef";

fn dummy_req() -> RepairRequest {
    RepairRequest {
        job_id: JOB.to_string(),
        user_id: "user_01".to_string(),
        idempotency_key: "ingest:user_01:upload".to_string(),
        par2_set_id: "par2-set-01".to_string(),
        missing_blocks: 3,
        round: 1,
        scratch_prefix: format!("ingest-scratch/{JOB}/r1/"),
    }
}

fn manifest() -> ScratchManifest {
    ScratchManifest {
        staging_id: "staging-0000000000000001".to_string(),
        files: vec![
            ScratchFile {
                file_name: "track.flac".to_string(),
                key: object_key_for_file_name("track.flac"),
                size: 7,
                role: FileRole::Data,
            },
            ScratchFile {
                file_name: "set.par2".to_string(),
                key: object_key_for_file_name("set.par2"),
                size: 8,
                role: FileRole::Par2,
            },
        ],
    }
}

fn valid_result_bytes(digest: &str) -> Vec<u8> {
    let result = RepairResult {
        status: RepairStatus::Repaired,
        exit_code: Some(0),
        files: vec![OutputFile {
            file_name: "track.flac".to_string(),
            key: object_key_for_file_name("track.flac"),
            size: 7,
        }],
        manifest_digest: digest.to_string(),
    };
    serde_json::to_vec(&result).unwrap()
}

/// Pad a valid JSON body with trailing whitespace (insignificant to the
/// parser) to an exact total length.
fn padded_to(base: &[u8], target_len: usize) -> Vec<u8> {
    assert!(
        base.len() <= target_len,
        "base body is {} bytes, over target {target_len}",
        base.len()
    );
    let mut body = base.to_vec();
    body.resize(target_len, b' ');
    body
}

#[test]
fn stored_result_one_byte_over_cap_is_500_before_parse() {
    let req = dummy_req();
    let digest = manifest_digest(&manifest());
    let base = valid_result_bytes(&digest);
    assert!(
        base.len() < MAX_RESULT_BYTES,
        "pin sensitivity: valid fixture is {} bytes, cap is {MAX_RESULT_BYTES}",
        base.len()
    );
    // Still valid JSON after padding, so only the size gate can reject it.
    let body = padded_to(&base, MAX_RESULT_BYTES + 1);
    assert_eq!(body.len(), MAX_RESULT_BYTES + 1);
    serde_json::from_slice::<RepairResult>(&body).unwrap();
    let err = plan(&req, Some(&body), &digest).unwrap_err();
    assert_eq!((err.status, err.code), (500, "repair_unavailable"));
}

#[test]
fn corrupt_stored_result_is_same_500() {
    let req = dummy_req();
    let digest = manifest_digest(&manifest());
    let body = b"{not json".to_vec();
    assert!(body.len() < MAX_RESULT_BYTES);
    let err = plan(&req, Some(&body), &digest).unwrap_err();
    assert_eq!((err.status, err.code), (500, "repair_unavailable"));
}

#[test]
fn under_cap_valid_result_with_matching_digest_is_already_done() {
    let req = dummy_req();
    let digest = manifest_digest(&manifest());
    let body = valid_result_bytes(&digest);
    assert!(
        body.len() < MAX_RESULT_BYTES,
        "pin sensitivity: valid fixture is {} bytes, cap is {MAX_RESULT_BYTES}",
        body.len()
    );
    match plan(&req, Some(&body), &digest).unwrap() {
        Plan::AlreadyDone(result) => {
            assert_eq!(result.manifest_digest, digest);
            assert_eq!(result.status, RepairStatus::Repaired);
        }
        other => panic!("expected AlreadyDone, got {other:?}"),
    }
}

#[test]
fn exactly_at_cap_invalid_json_still_500s() {
    let req = dummy_req();
    let digest = manifest_digest(&manifest());
    let body = vec![b'x'; MAX_RESULT_BYTES];
    assert_eq!(body.len(), MAX_RESULT_BYTES);
    let err = plan(&req, Some(&body), &digest).unwrap_err();
    assert_eq!((err.status, err.code), (500, "repair_unavailable"));
}

#[test]
fn valid_stored_result_exactly_at_cap_is_already_done() {
    let req = dummy_req();
    let digest = manifest_digest(&manifest());
    let base = valid_result_bytes(&digest);
    let body = padded_to(&base, MAX_RESULT_BYTES);
    assert_eq!(body.len(), MAX_RESULT_BYTES);
    serde_json::from_slice::<RepairResult>(&body).unwrap();

    match plan(&req, Some(&body), &digest).unwrap() {
        Plan::AlreadyDone(result) => {
            assert_eq!(result.manifest_digest, digest);
            assert_eq!(result.status, RepairStatus::Repaired);
        }
        other => panic!("expected AlreadyDone, got {other:?}"),
    }
}
