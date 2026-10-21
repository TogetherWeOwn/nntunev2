//! Body-cap and round-zero pins for the repair Worker contract.
//!
//! [`parse_request`] rejects bodies over [`MAX_REQUEST_BYTES`] with 413
//! before JSON parse, and [`parse_manifest`] rejects bodies over
//! [`MAX_MANIFEST_BYTES`] with 413; repair rounds are 1-based, so
//! `round == 0` is a 400 while `1..=3` pass with an otherwise-valid
//! request.

use nntune_core::par2::scratch::object_key_for_file_name;
use nntune_repair::contract::{
    parse_manifest, parse_request, MAX_MANIFEST_BYTES, MAX_REQUEST_BYTES,
};

const JOB: &str = "job_0123456789abcdef";

fn request_json_for_round(round: u32) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "job_id": JOB,
        "user_id": "user_01",
        "idempotency_key": format!("ingest:user_01:{}:upload", "a".repeat(64)),
        "par2_set_id": "par2-set-01",
        "missing_blocks": 3,
        "round": round,
        "scratch_prefix": format!("ingest-scratch/{JOB}/r{round}/"),
    }))
    .unwrap()
}

fn manifest_json() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "staging_id": "staging-0000000000000001",
        "files": [
            {"file_name": "track.flac", "key": object_key_for_file_name("track.flac"), "size": 7, "role": "data"},
            {"file_name": "set.par2", "key": object_key_for_file_name("set.par2"), "size": 8, "role": "par2"},
        ]
    }))
    .unwrap()
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
fn request_body_one_byte_over_cap_is_413() {
    let body = padded_to(&request_json_for_round(1), MAX_REQUEST_BYTES + 1);
    assert_eq!(body.len(), MAX_REQUEST_BYTES + 1);
    let err = parse_request(&body).unwrap_err();
    assert_eq!((err.status, err.code), (413, "payload_too_large"));
}

#[test]
fn request_body_exactly_at_cap_parses() {
    let body = padded_to(&request_json_for_round(1), MAX_REQUEST_BYTES);
    assert_eq!(body.len(), MAX_REQUEST_BYTES);
    let req = parse_request(&body).unwrap();
    assert_eq!(req.round, 1);
}

#[test]
fn manifest_body_one_byte_over_cap_is_413() {
    let body = padded_to(&manifest_json(), MAX_MANIFEST_BYTES + 1);
    assert_eq!(body.len(), MAX_MANIFEST_BYTES + 1);
    let err = parse_manifest(&body).unwrap_err();
    assert_eq!((err.status, err.code), (413, "payload_too_large"));
}

#[test]
fn manifest_body_exactly_at_cap_parses() {
    let body = padded_to(&manifest_json(), MAX_MANIFEST_BYTES);
    assert_eq!(body.len(), MAX_MANIFEST_BYTES);
    assert!(parse_manifest(&body).is_ok());
}

#[test]
fn round_zero_is_rejected_while_one_through_three_pass() {
    // Rounds are 1-based: round 0 has no canonical scratch prefix, so
    // the natural `r0` form is rejected as an invalid request.
    let zero = serde_json::to_vec(&serde_json::json!({
        "job_id": JOB,
        "user_id": "user_01",
        "idempotency_key": format!("ingest:user_01:{}:upload", "a".repeat(64)),
        "par2_set_id": "par2-set-01",
        "missing_blocks": 3,
        "round": 0,
        "scratch_prefix": format!("ingest-scratch/{JOB}/r0/"),
    }))
    .unwrap();
    let err = parse_request(&zero).unwrap_err();
    assert_eq!((err.status, err.code), (400, "invalid_request"));

    for round in 1..=3 {
        let req = parse_request(&request_json_for_round(round)).unwrap();
        assert_eq!(req.round, round);
    }
}
