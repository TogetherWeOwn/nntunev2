//! Staging cleanup: one staging is removed without touching its
//! siblings, cleanup is repeatable, and an unknown staging cleans
//! successfully (the Worker cleans before replacing a set it may
//! never have staged).

mod common;

use common::*;

fn cleanup(h: &Harness, job: &str, staging: &str) -> (u16, serde_json::Value) {
    h.post_json(
        "/cleanup",
        &serde_json::json!({"job_id": job, "staging_id": staging}).to_string(),
    )
}

#[test]
fn cleanup_removes_one_staging_and_leaves_siblings() {
    let h = Harness::start();
    let source = stage_input(&h, "job_clean01", HEX_A, b"bytes-a");
    let source_b = stage_input(&h, "job_clean01", HEX_B, b"bytes-b");
    let (status, _) = h.post_json(
        "/transcode",
        &transcode_body("job_clean01", HEX_A, &source, None),
    );
    assert_eq!(status, 200);
    let (status, _) = h.post_json(
        "/transcode",
        &transcode_body("job_clean01", HEX_B, &source_b, None),
    );
    assert_eq!(status, 200);

    let (status, reply) = cleanup(&h, "job_clean01", HEX_A);
    assert_eq!(status, 200);
    assert_eq!(reply, serde_json::json!({"ok": true}));

    let (status, _) = h.get_bytes(&format!("/jobs/job_clean01/t/{HEX_A}/out/artifact.ogg"));
    assert_eq!(status, 404);
    // The sibling staging is untouched: its artifact still serves and
    // its result still replays.
    let (status, _) = h.get_bytes(&format!("/jobs/job_clean01/t/{HEX_B}/out/artifact.ogg"));
    assert_eq!(status, 200);
    let (status, reply) = h.post_json(
        "/transcode",
        &transcode_body("job_clean01", HEX_B, &source_b, None),
    );
    assert_eq!(status, 200);
    assert_eq!(reply["status"], "transcoded");
}

#[test]
fn cleanup_is_repeatable_and_allows_restaging() {
    let h = Harness::start();
    let (status, _) = cleanup(&h, "job_clean02", HEX_A);
    assert_eq!(status, 200);
    let source = stage_input(&h, "job_clean02", HEX_A, b"bytes");
    let (status, _) = h.post_json(
        "/transcode",
        &transcode_body("job_clean02", HEX_A, &source, None),
    );
    assert_eq!(status, 200);
    let (status, _) = cleanup(&h, "job_clean02", HEX_A);
    assert_eq!(status, 200);
    let (status, _) = cleanup(&h, "job_clean02", HEX_A);
    assert_eq!(status, 200);
    // Fresh staging under the cleaned id encodes again (new
    // invocation, not a replay of removed state).
    let before = h.invocation_count();
    let source = stage_input(&h, "job_clean02", HEX_A, b"bytes");
    let (status, reply) = h.post_json(
        "/transcode",
        &transcode_body("job_clean02", HEX_A, &source, None),
    );
    assert_eq!(status, 200);
    assert_eq!(reply["status"], "transcoded");
    assert_eq!(h.invocation_count(), before + 1);
}

#[test]
fn cleanup_validates_identity() {
    let h = Harness::start();
    let (status, reply) = h.post_json("/cleanup", r#"{"job_id":"../x"}"#);
    assert_eq!(status, 400);
    assert_eq!(reply["error"], "invalid_request");
    let (status, reply) = h.post_json("/cleanup", r#"{"job_id":"job_01"}"#);
    assert_eq!(status, 400);
    assert_eq!(reply["error"], "invalid_request");
}
