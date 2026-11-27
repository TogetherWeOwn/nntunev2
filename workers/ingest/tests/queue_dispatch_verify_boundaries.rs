use nntune_ingest::api_error::ApiError;
use nntune_ingest::dispatch::{build_upload_dispatch, make_job_id};
use nntune_ingest::fixtures::SMALL_NZB;
use nntune_ingest::ingest_jobs::validate_admit_params;
use nntune_ingest::jobs::derive_idempotency_key;
use nntune_ingest::parse::ParsedFile;
use nntune_ingest::queue_consumer::validate_queue_msg;
use nntune_ingest::verify::{
    run_verifying_step, ArticleFetch, BodySource, ScratchStore, StagedFile, StagedSpoolSegment,
    VerifyCaps, VerifyError, VerifyProgress,
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
fn admission_and_queue_bind_the_owner_job_and_content() {
    let msg = build_upload_dispatch("member_1", SMALL_NZB).unwrap();
    let validate = |id, user, key| validate_admit_params(id, user, key, 0);
    assert!(validate(&msg.job_id, &msg.user_id, &msg.idempotency_key).is_ok());
    assert!(validate("job_wrong", &msg.user_id, &msg.idempotency_key).is_err());
    assert!(validate(&msg.job_id, "member_2", &msg.idempotency_key).is_err());
    assert!(validate(&msg.job_id, &msg.user_id, "ingest:member_1:bad:upload").is_err());
    assert!(validate_queue_msg(&msg).is_ok());
    assert!(msg.matches_upload_payload(SMALL_NZB.as_bytes()));
    assert!(!msg.matches_upload_payload(format!("{SMALL_NZB}\n").as_bytes()));

    let mut tampered = msg;
    tampered.payload_ref = tampered.payload_ref.map(|value| value.to_ascii_uppercase());
    assert!(validate_queue_msg(&tampered).is_err());
    assert!(!tampered.matches_upload_payload(SMALL_NZB.as_bytes()));
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

struct NoIo;

impl BodySource for NoIo {
    async fn fetch_body(&mut self, _: &str) -> Result<ArticleFetch, VerifyError> {
        panic!("unexpected provider read")
    }
}

impl ScratchStore for NoIo {
    async fn head(&self, _: &str) -> Result<Option<StagedFile>, VerifyError> {
        panic!("unexpected scratch head")
    }

    async fn get(&self, _: &str) -> Result<Option<Vec<u8>>, VerifyError> {
        panic!("unexpected scratch read")
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

fn cursor(key: String) -> VerifyProgress {
    let mut progress = VerifyProgress::new(1);
    progress.spool.insert(
        0,
        vec![StagedSpoolSegment {
            key,
            size: 1,
            crc32: 0,
            missing: false,
            size_refused: false,
        }],
    );
    progress
}

#[tokio::test]
async fn verify_rejects_bad_inventory_and_foreign_cursor_before_io() {
    let mut empty = file();
    empty.segment_ids.clear();
    let foreign = VerifyProgress::spool_key("job_other", 1, 0, 0);
    let cases = [
        (
            empty,
            VerifyProgress::new(1),
            "invalid file segment inventory",
        ),
        (file(), cursor(foreign), "invalid scratch progress"),
    ];
    for (file, mut progress, message) in cases {
        assert_eq!(
            run_verifying_step(
                JOB,
                &[file],
                &mut progress,
                &mut NoIo,
                &NoIo,
                &VerifyCaps::default(),
                &|| false
            )
            .await,
            Err(VerifyError::Permanent(message.into()))
        );
    }
}
