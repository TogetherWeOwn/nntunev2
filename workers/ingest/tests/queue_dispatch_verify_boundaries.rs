use nntune_ingest::api_error::ApiError;
use nntune_ingest::dispatch::{build_upload_dispatch, make_job_id};
use nntune_ingest::fixtures::SMALL_NZB;
use nntune_ingest::ingest_jobs::validate_admit_params;
use nntune_ingest::jobs::derive_idempotency_key;
use nntune_ingest::parse::ParsedFile;
use nntune_ingest::queue_consumer::validate_match_payload;
use nntune_ingest::retry::ERR_MATCH_FAILED;
use nntune_ingest::verify::{
    run_verifying_step, ArticleFetch, BodySource, ScratchStore, StagedFile, StagedSpoolSegment,
    VerifyCaps, VerifyError, VerifyOutcome, VerifyProgress,
};

#[test]
fn wire_error_redacts_mutated_server_details() {
    let mut error = ApiError::service_unavailable("internal detail");
    error.message = "scratch binding detail".into();
    error.retryable = false;
    assert_eq!(
        serde_json::to_value(&error).unwrap(),
        serde_json::json!({"code": "service_unavailable", "message": "service unavailable", "retryable": true})
    );
    error.code = "invalid_request";
    assert_eq!(error.body_json(), ApiError::internal_error("").body_json());
}

#[test]
fn admission_binds_the_owner_and_job() {
    let msg = build_upload_dispatch("member_1", SMALL_NZB).unwrap();
    let validate = |id, user, key| validate_admit_params(id, user, key, 0);
    assert!(validate(&msg.job_id, &msg.user_id, &msg.idempotency_key).is_ok());
    assert!(validate("job_wrong", &msg.user_id, &msg.idempotency_key).is_err());
    assert!(validate(&msg.job_id, "member_2", &msg.idempotency_key).is_err());
    assert!(validate(&msg.job_id, &msg.user_id, "ingest:member_1:bad:upload").is_err());
}

#[test]
fn matching_retries_absence_and_rejects_replaced_bytes() {
    let msg = build_upload_dispatch("member_1", SMALL_NZB).unwrap();
    let present = validate_match_payload(&msg, Some(SMALL_NZB.into())).unwrap();
    assert_eq!(present, SMALL_NZB);
    for (payload, retryable) in [(None, true), (Some(format!("{SMALL_NZB}\n")), false)] {
        let error = validate_match_payload(&msg, payload).unwrap_err();
        assert_eq!((error.code, error.transient), (ERR_MATCH_FAILED, retryable));
    }
    let (_, load) = include_str!("../src/queue_consumer.rs")
        .split_once("async fn load_payload_for_match(")
        .unwrap();
    assert!(load
        .split_once("\n}\n")
        .unwrap()
        .0
        .contains("validate_match_payload(msg, payload)"));
}

#[test]
fn queue_checks_identity_before_logging() {
    let source = include_str!("../src/queue_consumer.rs");
    let (_, delivery) = source.split_once("async fn process_raw_message(").unwrap();
    let (before_log, _) = delivery.split_once("\"ingest.job.consumed\"").unwrap();
    assert!(before_log.contains("if validate_queue_msg(&msg).is_err()"));
}

#[test]
fn job_id_and_key_use_the_same_digest_canonicalization() {
    let upper = "A".repeat(64);
    let lower = upper.to_ascii_lowercase();
    let canonical = derive_idempotency_key("member_1", &upper, "upload").unwrap();
    let id = make_job_id("member_1", &upper, "upload");
    assert_eq!(id, make_job_id("member_1", &lower, "upload"));
    assert!(validate_admit_params(&id, "member_1", &canonical, 0).is_ok());
}

const JOB: &str = "job_fixture";

#[derive(Default)]
struct Scratch(Option<(String, StagedFile)>);

impl BodySource for Scratch {
    async fn fetch_body(&mut self, _: &str) -> Result<ArticleFetch, VerifyError> {
        panic!("unexpected provider read")
    }
}

impl ScratchStore for Scratch {
    async fn head(&self, key: &str) -> Result<Option<StagedFile>, VerifyError> {
        let (stored, meta) = self.0.as_ref().expect("unexpected scratch head");
        Ok((stored == key).then(|| meta.clone()))
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, VerifyError> {
        let meta = self.head(key).await?.expect("unexpected scratch read");
        Ok(Some(serde_json::to_vec(&meta).unwrap()))
    }

    async fn list_keys(&self, _: &str) -> Result<Vec<String>, VerifyError> {
        panic!("unexpected scratch listing")
    }

    async fn put(&self, _: &str, _: Vec<u8>, _: &StagedFile) -> Result<(), VerifyError> {
        panic!("unexpected scratch write")
    }
}

fn file() -> ParsedFile {
    ParsedFile {
        name: "track.flac".into(),
        is_par2: false,
        segment_ids: vec!["track@synthetic.invalid".into()],
        segment_bytes: vec![None],
    }
}

fn staged(key: String) -> StagedFile {
    serde_json::from_value(serde_json::json!({"key": key, "size": 1, "sha256": "fixture"})).unwrap()
}

fn segment(key: String) -> StagedSpoolSegment {
    StagedSpoolSegment {
        key,
        size: 0,
        crc32: 0,
        missing: true,
        size_refused: false,
    }
}

async fn verify(
    file: ParsedFile,
    progress: &mut VerifyProgress,
    scratch: &Scratch,
) -> Result<VerifyOutcome, VerifyError> {
    let caps = VerifyCaps {
        max_files_per_delivery: 0,
        ..VerifyCaps::default()
    };
    run_verifying_step(
        JOB,
        &[file],
        progress,
        &mut Scratch::default(),
        scratch,
        &caps,
        &|| false,
    )
    .await
}

#[tokio::test]
async fn verify_rejects_bad_inventory_and_foreign_cursor_before_io() {
    let mut empty = file();
    empty.segment_ids.clear();
    empty.segment_bytes.clear();
    let mut mismatched = file();
    mismatched.segment_bytes.clear();
    let foreign = VerifyProgress::spool_key("job_other", 1, 0, 0);
    let mut cursor = VerifyProgress::new(1);
    cursor.spool.insert(0, vec![segment(foreign.clone())]);
    let mut staged_file = VerifyProgress::new(1);
    staged_file.staged.insert(0, staged(foreign));
    let inventory_error = "invalid file segment inventory";
    let cases = [
        (empty, VerifyProgress::new(1), inventory_error),
        (mismatched, VerifyProgress::new(1), inventory_error),
        (file(), cursor, "invalid scratch progress"),
        (file(), staged_file, "invalid scratch progress"),
    ];
    for (file, mut progress, message) in cases {
        assert_eq!(
            verify(file, &mut progress, &Scratch::default()).await,
            Err(VerifyError::Permanent(message.into()))
        );
    }
}

#[tokio::test]
async fn verify_ignores_invalid_scratch_adoption_records() {
    let file_key = VerifyProgress::file_key(JOB, 1, 0, &file().name);
    let spool_index = VerifyProgress::spool_meta_key(JOB, 1, 0);
    let spool_key = VerifyProgress::spool_key(JOB, 1, 0, 0);
    let mut assembled = staged(file_key.clone());
    assembled.spool.push(segment(spool_key.clone()));
    let mut foreign_index = staged(spool_index.clone());
    foreign_index
        .spool
        .push(segment(VerifyProgress::spool_key("job_other", 1, 0, 0)));
    let mut missing = staged(spool_key.clone());
    missing.missing = true;
    let cases = [
        (file_key, assembled),
        (spool_index.clone(), staged(spool_index.clone())),
        (spool_index, foreign_index),
        (spool_key.clone(), staged("other-key".into())),
        (spool_key, missing),
    ];
    for row in cases {
        let mut progress = VerifyProgress::new(1);
        assert_eq!(
            verify(file(), &mut progress, &Scratch(Some(row))).await,
            Ok(VerifyOutcome::Incomplete)
        );
        assert!(progress.staged.is_empty() && progress.spool.is_empty());
    }
}
