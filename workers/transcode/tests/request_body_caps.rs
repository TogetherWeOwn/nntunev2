//! Request body caps: the 64 KiB request ceiling (exact-cap
//! admissible, cap+1 rejected) and per-field rejection boundaries.

use nntune_transcode::contract::{parse_request, ApiFault, MAX_REQUEST_BYTES};

const JOB: &str = "job_0123456789abcdef";
const HEX64: &str = "9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734";
const PREFIX: &str = "ingest-scratch/job_0123456789abcdef/r1/";

fn body_with_user(user_id: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "job_id": JOB,
        "user_id": user_id,
        "idempotency_key": format!("ingest:{user_id}:{}:upload", "a".repeat(64)),
        "input_key": format!("0007-{HEX64}"),
        "source_sha256": HEX64,
        "scratch_prefix": PREFIX,
        "staging_id": HEX64,
    }))
    .unwrap()
}

#[test]
fn oversize_body_rejects_from_length() {
    // Pad past the ceiling. Length is checked before field
    // validation, so this 413s even though the padded user id would
    // also fail its own boundary.
    let mut body = body_with_user("user_01");
    assert!(body.len() < MAX_REQUEST_BYTES);
    let pad = MAX_REQUEST_BYTES - body.len() + 1;
    let big_user = format!("user_01{}", "x".repeat(pad));
    body = body_with_user(&big_user);
    assert!(body.len() > MAX_REQUEST_BYTES);
    assert_eq!(
        parse_request(&body),
        Err(ApiFault::new(413, "payload_too_large"))
    );
}

#[test]
fn exact_cap_body_parses() {
    let base = body_with_user("u");
    let pad = MAX_REQUEST_BYTES - base.len();
    // Grow the user id to land exactly on the cap: user ids over 128
    // chars reject, so pad inside a field the parser ignores instead.
    // Unknown fields are ignored by serde, which is exactly what lets
    // this test isolate the byte ceiling from field validation.
    let mut value: serde_json::Value = serde_json::from_slice(&base).unwrap();
    value["padding"] = "x".repeat(pad - 15).into();
    let mut body = serde_json::to_vec(&value).unwrap();
    // Nudge to the exact boundary (key overhead is constant).
    while body.len() < MAX_REQUEST_BYTES {
        value["padding"] = format!("{}x", value["padding"].as_str().unwrap()).into();
        body = serde_json::to_vec(&value).unwrap();
    }
    assert_eq!(body.len(), MAX_REQUEST_BYTES);
    assert!(parse_request(&body).is_ok());
}

#[test]
fn field_boundaries_reject() {
    // user: empty, >128, colon, NUL.
    assert!(parse_request(&body_with_user("")).is_err());
    assert!(parse_request(&body_with_user(&"u".repeat(129))).is_err());
    assert!(parse_request(&body_with_user("user:01")).is_err());
    assert!(parse_request(&body_with_user("user\0")).is_err());
    assert!(parse_request(&body_with_user(&"u".repeat(128))).is_ok());
    // job: empty, >64, hostile charset.
    let job_body = |job_id: &str| {
        serde_json::to_vec(&serde_json::json!({
            "job_id": job_id,
            "user_id": "user_01",
            "idempotency_key": "ingest:user_01:abc:upload",
            "input_key": format!("0007-{HEX64}"),
            "source_sha256": HEX64,
            "scratch_prefix": format!("ingest-scratch/{job_id}/r1/"),
            "staging_id": HEX64,
        }))
        .unwrap()
    };
    assert!(parse_request(&job_body("")).is_err());
    assert!(parse_request(&job_body(&"j".repeat(65))).is_err());
    assert!(parse_request(&job_body("job 01")).is_err());
    assert!(parse_request(&job_body(&"j".repeat(64))).is_ok());
    // key: empty, >768, wrong shape, NUL.
    let key_body = |key: &str| {
        serde_json::to_vec(&serde_json::json!({
            "job_id": JOB,
            "user_id": "user_01",
            "idempotency_key": key,
            "input_key": format!("0007-{HEX64}"),
            "source_sha256": HEX64,
            "scratch_prefix": PREFIX,
            "staging_id": HEX64,
        }))
        .unwrap()
    };
    assert!(parse_request(&key_body("")).is_err());
    assert!(parse_request(&key_body(&format!("ingest:{}", "k".repeat(762)))).is_err());
    assert!(parse_request(&key_body("upload:user_01:abc")).is_err());
    assert!(parse_request(&key_body("ingest:user\0:abc")).is_err());
    assert!(parse_request(&key_body(&format!("ingest:{}", "k".repeat(761)))).is_ok());
}
