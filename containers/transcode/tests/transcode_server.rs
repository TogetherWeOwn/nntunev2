//! HTTP-level transcode roundtrips against a live server with a stub
//! encoder. The stub stands in for the image's `ffmpeg`: it copies a
//! canned Ogg page (or junk, or nothing) to the output path
//! (`ffmpeg_argv`'s last argv) and exits per its mode file, so every
//! terminal and transient server outcome is exercised without an
//! encoder binary. Real-`ffmpeg` determinism is proven by the image
//! self-test (`tests/determinism_self_test.sh` in CI).

mod common;

use common::*;
use nntune_core::par2::scratch::sha256_hex;
use nntune_core::transcode::{normalize_ogg_serial, OGG_FIXED_SERIAL};

#[test]
fn health_reports_ok() {
    let h = Harness::start();
    let (status, body) = h.get_bytes("/health");
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({"ok": true})
    );
}

#[test]
fn put_post_get_roundtrip_normalises_and_hashes() {
    let h = Harness::start();
    let source = stage_input(&h, "job_roundtrip01", HEX_A, b"fake-flac-bytes");
    let (status, reply) = h.post_json(
        "/transcode",
        &transcode_body("job_roundtrip01", HEX_A, &source, None),
    );
    assert_eq!(status, 200, "POST /transcode: {reply:?}");
    assert_eq!(reply["status"], "transcoded");
    assert_eq!(reply["exit_code"], 0);

    let expected = normalize_ogg_serial(CANNED_PAGE, OGG_FIXED_SERIAL).unwrap();
    assert_ne!(expected, CANNED_PAGE.to_vec());
    assert_eq!(reply["artifact_sha256"], sha256_hex(&expected));
    assert_eq!(reply["byte_size"], expected.len() as u64);

    let (status, fetched) =
        h.get_bytes(&format!("/jobs/job_roundtrip01/t/{HEX_A}/out/artifact.ogg"));
    assert_eq!(status, 200);
    assert_eq!(fetched, expected);
    // Fixed serial, idempotent normalisation.
    assert_eq!(&fetched[14..18], &1u32.to_le_bytes());
    assert_eq!(
        normalize_ogg_serial(&fetched, OGG_FIXED_SERIAL).unwrap(),
        fetched
    );
    assert_eq!(h.invocation_count(), 1);
}

#[test]
fn identical_post_replays_without_rerunning() {
    let h = Harness::start();
    let source = stage_input(&h, "job_replay01", HEX_A, b"bytes");
    let body = transcode_body("job_replay01", HEX_A, &source, None);
    let (first_status, first) = h.post_json("/transcode", &body);
    assert_eq!(first_status, 200);
    let (second_status, second) = h.post_json("/transcode", &body);
    assert_eq!(second_status, 200);
    assert_eq!(first, second);
    assert_eq!(h.invocation_count(), 1);
}

#[test]
fn new_staging_reruns_on_restaged_bytes() {
    let h = Harness::start();
    let source = stage_input(&h, "job_rerun01", HEX_A, b"bytes");
    let (status, first) = h.post_json(
        "/transcode",
        &transcode_body("job_rerun01", HEX_A, &source, None),
    );
    assert_eq!(status, 200);
    let source_b = stage_input(&h, "job_rerun01", HEX_B, b"bytes");
    assert_eq!(source_b, source);
    let (status, second) = h.post_json(
        "/transcode",
        &transcode_body("job_rerun01", HEX_B, &source_b, None),
    );
    assert_eq!(status, 200);
    assert_eq!(first, second);
    assert_eq!(h.invocation_count(), 2);
}

#[test]
fn different_input_for_one_staging_conflicts() {
    let h = Harness::start();
    let source = stage_input(&h, "job_conflict01", HEX_A, b"bytes");
    let (status, _) = h.post_json(
        "/transcode",
        &transcode_body("job_conflict01", HEX_A, &source, None),
    );
    assert_eq!(status, 200);
    let other_key = format!("0008-{HEX_B}");
    let (status, _) = h.put(
        &format!("/jobs/job_conflict01/t/{HEX_A}/in/{other_key}"),
        b"other",
    );
    assert_eq!(status, 201);
    let body = serde_json::json!({
        "job_id": "job_conflict01",
        "staging_id": HEX_A,
        "input_key": other_key,
        "source_sha256": sha256_hex(b"other"),
    })
    .to_string();
    let (status, reply) = h.post_json("/transcode", &body);
    assert_eq!(status, 409);
    assert_eq!(reply["error"], "transcode_input_conflict");
}

#[test]
fn missing_or_wrong_input_fails_the_staging_check() {
    let h = Harness::start();
    // No PUT at all.
    let (status, reply) = h.post_json(
        "/transcode",
        &transcode_body("job_nostage01", HEX_A, &sha256_hex(b"bytes"), None),
    );
    assert_eq!(status, 400);
    assert_eq!(reply["error"], "scratch_input_mismatch");
    // Staged bytes whose hash the request misstates.
    let source = stage_input(&h, "job_badhash01", HEX_A, b"bytes");
    assert_ne!(source, HEX_B);
    let (status, reply) = h.post_json(
        "/transcode",
        &transcode_body("job_badhash01", HEX_A, HEX_B, None),
    );
    assert_eq!(status, 400);
    assert_eq!(reply["error"], "scratch_input_mismatch");
    assert_eq!(h.invocation_count(), 0);
}

#[test]
fn nonzero_exit_is_terminal_and_replayed() {
    let h = Harness::start();
    h.set_mode("fail");
    let source = stage_input(&h, "job_untrans01", HEX_A, b"bytes");
    let body = transcode_body("job_untrans01", HEX_A, &source, None);
    let (status, reply) = h.post_json("/transcode", &body);
    assert_eq!(status, 200);
    assert_eq!(reply["status"], "untranscodable");
    assert_eq!(reply["exit_code"], 1);
    assert!(reply["artifact_sha256"].is_null());
    let (status, again) = h.post_json("/transcode", &body);
    assert_eq!(status, 200);
    assert_eq!(again, reply);
    assert_eq!(h.invocation_count(), 1);
    let (status, _) = h.get_bytes(&format!("/jobs/job_untrans01/t/{HEX_A}/out/artifact.ogg"));
    assert_eq!(status, 404);
}

#[test]
fn non_ogg_and_oversize_outputs_are_untranscodable() {
    let h = Harness::start();
    h.set_mode("junk");
    let source = stage_input(&h, "job_junk01", HEX_A, b"bytes");
    let (status, reply) = h.post_json(
        "/transcode",
        &transcode_body("job_junk01", HEX_A, &source, None),
    );
    assert_eq!(status, 200);
    assert_eq!(reply["status"], "untranscodable");

    h.set_mode("big");
    let source = stage_input(&h, "job_big01", HEX_A, b"bytes");
    let (status, reply) = h.post_json(
        "/transcode",
        &transcode_body("job_big01", HEX_A, &source, None),
    );
    assert_eq!(status, 200);
    assert_eq!(reply["status"], "untranscodable");
}

#[test]
fn encode_timeout_is_retryable_and_persists_nothing() {
    let h = Harness::start();
    h.set_mode("slow");
    let source = stage_input(&h, "job_timeout01", HEX_A, b"bytes");
    let (status, reply) = h.post_json(
        "/transcode",
        &transcode_body("job_timeout01", HEX_A, &source, Some(1)),
    );
    assert_eq!(status, 503);
    assert_eq!(reply["error"], "transcode_timeout");
    let (status, _) = h.get_bytes(&format!("/jobs/job_timeout01/t/{HEX_A}/out/artifact.ogg"));
    assert_eq!(status, 404);
    // The timed-out run persisted no verdict: the same staging encodes
    // once the encoder is fast again.
    h.set_mode("ok");
    let (status, reply) = h.post_json(
        "/transcode",
        &transcode_body("job_timeout01", HEX_A, &source, None),
    );
    assert_eq!(status, 200);
    assert_eq!(reply["status"], "transcoded");
}

#[test]
fn malformed_requests_reject_without_running() {
    let h = Harness::start();
    let (status, reply) = h.post_json("/transcode", r#"{"job_id":"x"}"#);
    assert_eq!(status, 400);
    assert_eq!(reply["error"], "invalid_request");
    let (status, _) = h.raw(
        "POST /transcode HTTP/1.1\r\nHost: x\r\nContent-Length: 8\r\nConnection: close\r\n\r\n",
        b"not json",
    );
    assert_eq!(status, 400);
    let (status, _) = h.get_bytes("/jobs/job_01/t/staging_09/out/other.bin");
    assert_eq!(status, 404);
    let (status, _) = h.get_bytes("/nope");
    assert_eq!(status, 404);
    assert_eq!(h.invocation_count(), 0);
}
