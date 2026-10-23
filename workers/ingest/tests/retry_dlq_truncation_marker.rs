//! DLQ truncation ellipsis-marker + char-boundary pin.
//!
//! Contract under test (`workers/ingest/src/retry.rs:89-99`
//! `bounded_redacted_detail`):
//!
//! - the first [`MAX_DLQ_DETAIL_CHARS`] (512) chars are kept on a char
//!   boundary, then a single `…` (U+2026) marker is appended, so a
//!   truncated detail is exactly 513 chars long;
//! - truncation never splits a rune: an emoji straddling the 512-char
//!   boundary survives whole (a byte-slice at 512 bytes would cut its
//!   4-byte sequence);
//! - a short detail passes through byte-identical with no marker.
//!
//! The length cap alone is pinned in `tests/retry_dlq.rs`
//! (`quarantine_redacts_and_bounds_untrusted_error_details`); this file
//! pins the marker, the exact 513 length, and rune integrity, each of
//! which fails under a marker-removal mutant.

use nntune_ingest::dispatch::build_upload_dispatch;
use nntune_ingest::fixtures::SMALL_NZB;
use nntune_ingest::jobs::{JobCheckpoint, MAX_ATTEMPTS};
use nntune_ingest::queue::QueueMsg;
use nntune_ingest::retry::{
    decide_checkpoint, Disposition, JobError, ERR_PROBE_FAILED, MAX_DLQ_DETAIL_CHARS,
};

fn poison_msg() -> QueueMsg {
    build_upload_dispatch("poison_user", SMALL_NZB).unwrap()
}

fn checkpoint_for(msg: &QueueMsg) -> JobCheckpoint {
    JobCheckpoint {
        job_id: msg.job_id.clone(),
        user_id: msg.user_id.clone(),
        idempotency_key: msg.idempotency_key.clone(),
        source: msg.source,
        state: msg.state,
        attempt: MAX_ATTEMPTS,
        repair_rounds: msg.repair_rounds,
        probe_tally: None,
        par2_set_id: None,
        missing_blocks: 0,
        failure_code: None,
        failure_reason: None,
        provider_id: msg.provider_id.clone(),
        transcode_inputs: Vec::new(),
        transcode_next: 0,
    }
}

/// 600-char detail with a 4-byte emoji as the 512th char (index 511), so a
/// naive 512-byte cut would split its byte sequence.
fn straddling_detail() -> String {
    format!("{}😀{}", "a".repeat(511), "b".repeat(88))
}

#[test]
fn truncation_appends_ellipsis_marker_on_a_char_boundary() {
    let detail = straddling_detail();
    assert_eq!(detail.chars().count(), 600);

    let msg = poison_msg();
    let mut checkpoint = checkpoint_for(&msg);
    let disposition = decide_checkpoint(
        &msg,
        &mut checkpoint,
        Err(JobError::permanent(ERR_PROBE_FAILED, detail.clone())),
    )
    .unwrap();
    let Disposition::Quarantine(entry) = disposition else {
        panic!("permanent failure must quarantine");
    };

    // The checkpoint failure reason is exactly the bounded detail.
    let bounded = checkpoint.failure_reason.as_ref().unwrap();
    assert_eq!(
        bounded.chars().count(),
        MAX_DLQ_DETAIL_CHARS + 1,
        "truncated detail must be 512 kept chars + one ellipsis marker"
    );
    assert!(
        bounded.ends_with('…'),
        "truncated detail must end in the ellipsis marker"
    );

    // Char-boundary integrity: the first 512 chars survive whole, ending
    // with the intact emoji — no split rune, no replacement character.
    let kept: String = detail.chars().take(MAX_DLQ_DETAIL_CHARS).collect();
    let expected = format!("{kept}…");
    assert_eq!(*bounded, expected);
    assert!(bounded.contains('😀'));
    assert!(!bounded.contains('\u{FFFD}'));
    assert!(bounded.is_char_boundary(bounded.len() - '…'.len_utf8()));
    // The emoji is the last kept char (index 511), proving the boundary
    // kept a full rune instead of cutting its bytes.
    assert_eq!(kept.chars().last(), Some('😀'));

    // The DLQ entry reason carries the same bounded detail after its prefix.
    let suffix = entry
        .reason
        .rsplit_once(": ")
        .map(|(_, tail)| tail)
        .expect("quarantine reason must carry the detail after ': '");
    assert_eq!(suffix, *bounded);
}

#[test]
fn short_detail_passes_through_without_marker() {
    let short = "pool 430 on every segment, no par2 set — short ✓";
    assert!(short.chars().count() < MAX_DLQ_DETAIL_CHARS);

    let msg = poison_msg();
    let mut checkpoint = checkpoint_for(&msg);
    let disposition = decide_checkpoint(
        &msg,
        &mut checkpoint,
        Err(JobError::permanent(ERR_PROBE_FAILED, short)),
    )
    .unwrap();
    assert!(matches!(disposition, Disposition::Quarantine(_)));
    let bounded = checkpoint.failure_reason.as_ref().unwrap();
    assert_eq!(bounded, short);
    assert!(!bounded.ends_with('…'));
}
