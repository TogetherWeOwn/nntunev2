//! Prepared admission branch in disposable PostgreSQL, not ordered migration
//! acceptance or live dispatch. Reuse the existing synthetic corpus in memory.
use super::*;
use intake::{AdminIngestAttempt, AdminIngestUploadOutcome, FailureReason};
use nntune_api::{
    handlers::StoreError,
    worker_entry::admin_ingest_upload_for_postgres_test as upload_with_checkpoint,
};
use nntune_ingest::{
    admin_checkpoint::{Identity, Snapshot},
    admin_checkpoint_store::{self as checkpoints, StageOriginal},
    jobs::JobState,
};

fn fresh_checkpoint(job_id: &str, idempotency_key: &str) -> Result<String, StoreError> {
    Snapshot::new_admission(&Identity {
        job_id,
        user_id: "",
        idempotency_key,
    })
    .and_then(|snapshot| snapshot.encode())
    .map_err(|_| StoreError)
}

async fn upload(
    app: &mut tokio_postgres::Client,
    attempt: &AdminIngestAttempt,
    initialize_checkpoint: bool,
) -> Result<AdminIngestUploadOutcome, StoreError> {
    upload_with_checkpoint(
        app,
        attempt,
        if initialize_checkpoint {
            Some(fresh_checkpoint)
        } else {
            None
        },
    )
    .await
}

async fn database() -> tokio_postgres::Client {
    let app = client(false).await;
    app.batch_execute(
        "CREATE TEMP TABLE users (id TEXT PRIMARY KEY, role TEXT, deleting_at BIGINT);
         INSERT INTO users VALUES ('stage_user', 'admin', NULL);
         CREATE TEMP TABLE ingest_jobs (id TEXT PRIMARY KEY, user_id TEXT REFERENCES users(id), idempotency_key TEXT, accepted_at BIGINT, status TEXT, source TEXT, content_sha256 TEXT, nzb_bytes BIGINT, file_count INTEGER, segment_count INTEGER, failure_reason TEXT, admin_checkpoint JSONB, admin_checkpoint_revision BIGINT NOT NULL DEFAULT 0);
         CREATE TEMP TABLE ingest_originals (job_id TEXT PRIMARY KEY REFERENCES ingest_jobs(id), content_sha256 TEXT, nzb_bytes BIGINT, nzb_ref_encrypted BYTEA, nzb_ref_envelope_version SMALLINT, nzb_ref_kek_version INTEGER, nzb_ref_dek_wrapped_nonce BYTEA, nzb_ref_dek_wrapped BYTEA, nzb_ref_payload_nonce BYTEA, nzb_ref_payload_format SMALLINT, created_at BIGINT);
         CREATE TEMP TABLE audit_log (user_id TEXT, action TEXT, ok BOOLEAN, source_ip TEXT, at BIGINT, subject_user_id TEXT, details JSONB)",
    ).await.unwrap();
    app
}

fn attempt() -> AdminIngestAttempt {
    let body = nntune_ingest::fixtures::SMALL_NZB.as_bytes();
    let counts = intake::classify_upload(body).unwrap();
    let job_id = intake::derive_job_id(body);
    let hash = intake::content_sha256_hex(body);
    let original = intake::encrypt_ingest_original(&[9; 32], 7, &hash, &job_id, body).unwrap();
    AdminIngestAttempt {
        actor_user_id: "stage_user".into(),
        job_id,
        idempotency_key: format!("admin:{hash}"),
        content_sha256_hex: hash,
        nzb_bytes: body.len() as u64,
        file_count: Some(counts.file_count),
        segment_count: Some(counts.segment_count),
        failure: None,
        original: Some(original),
        kek_version: 7,
        source_ip: String::new(),
        now: 1,
    }
}

async fn counts(app: &tokio_postgres::Client) -> (i64, i64, i64) {
    let row = app.query_one(
        "SELECT (SELECT count(*) FROM ingest_jobs), (SELECT count(*) FROM ingest_originals), (SELECT count(*) FROM audit_log)", &[],
    ).await.unwrap();
    (row.get(0), row.get(1), row.get(2))
}

async fn authority(app: &tokio_postgres::Client) -> (String, i64, Option<String>) {
    let row = app
        .query_one(
            "SELECT status, admin_checkpoint_revision, admin_checkpoint::text FROM ingest_jobs",
            &[],
        )
        .await
        .unwrap();
    (row.get(0), row.get(1), row.get(2))
}

#[tokio::test]
async fn fresh_upload_commits_original_audit_and_loadable_owner_free_authority() {
    let mut app = database().await;
    let attempt = attempt();
    let result = upload(&mut app, &attempt, true).await.unwrap();
    assert!(matches!(
        result,
        AdminIngestUploadOutcome::Stored {
            deduplicated: false,
            ..
        }
    ));
    assert_eq!(counts(&app).await, (1, 1, 1));
    let original = read_original(
        &Authority,
        &mut PostgresOriginalRepository { client: &app },
        &Keys,
        &attempt.job_id,
    )
    .await
    .unwrap();
    let snapshot = checkpoints::load(
        &app,
        &StageOriginal {
            job_id: original.job_id(),
            idempotency_key: original.idempotency_key(),
            nzb_bytes: original.as_bytes().len(),
            parsed: original.parsed(),
        },
        1,
    )
    .await
    .unwrap();
    assert_eq!(
        (
            snapshot.revision(),
            snapshot.generation(),
            snapshot.status()
        ),
        (1, 1, "queued")
    );
    let cp = snapshot.checkpoint();
    assert_eq!((cp.state, cp.attempt), (JobState::Queued, 1));
    assert!(cp.user_id.is_empty());
    assert!(cp.admin_release_mbid.is_none() && cp.probe_tally.is_none());
    assert!(cp.failure_code.is_none() && cp.failure_reason.is_none());
    let audit = app
        .query_one("SELECT action, details FROM audit_log", &[])
        .await
        .unwrap();
    assert_eq!(
        audit.get::<_, String>(0),
        intake::INGEST_UPLOAD_AUDIT_ACTION
    );
    assert_eq!(
        audit.get::<_, serde_json::Value>(1),
        serde_json::json!({"job_id":attempt.job_id,"outcome":"accepted"})
    );
}

#[tokio::test]
async fn upload_replay_never_repairs_or_resets_any_existing_authority() {
    let mut app = database().await;
    let attempt = attempt();
    upload(&mut app, &attempt, true).await.unwrap();
    let initial = authority(&app).await.2.unwrap();
    let mut modified: serde_json::Value = serde_json::from_str(&initial).unwrap();
    modified["generation"] = 7.into();
    for (status, revision, encoded) in [
        ("queued", 0_i64, None),
        ("queued", 7, Some("null".to_string())),
        ("running", 7, Some(modified.to_string())),
        ("needs_review", 7, Some("{}".to_string())),
        ("needs_archive_read", 7, Some(initial.clone())),
        ("rejected", 7, Some(initial.clone())),
        ("done", 7, Some(initial)),
    ] {
        app.execute("UPDATE ingest_jobs SET status = $1, admin_checkpoint_revision = $2, admin_checkpoint = $3::text::jsonb", &[&status,&revision,&encoded]).await.unwrap();
        let before = authority(&app).await;
        let result = upload(&mut app, &attempt, true).await.unwrap();
        assert!(matches!(
            result,
            AdminIngestUploadOutcome::Stored {
                deduplicated: true,
                ..
            }
        ));
        assert_eq!(authority(&app).await, before);
    }
    assert_eq!(counts(&app).await, (1, 1, 8));
    let duplicate_audits: i64 = app
        .query_one(
            "SELECT count(*) FROM audit_log WHERE details->>'outcome' = 'duplicate'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(duplicate_audits, 7);
}

#[tokio::test]
async fn checkpoint_constructor_refusal_aborts_fresh_upload_but_replay_never_calls_it() {
    fn refuse(_: &str, _: &str) -> Result<String, StoreError> {
        Err(StoreError)
    }
    fn unexpected(_: &str, _: &str) -> Result<String, StoreError> {
        panic!("checkpoint constructor must not run")
    }
    let mut app = database().await;
    let attempt = attempt();
    assert!(upload_with_checkpoint(&mut app, &attempt, Some(refuse))
        .await
        .is_err());
    assert_eq!(counts(&app).await, (0, 0, 0));

    upload(&mut app, &attempt, true).await.unwrap();
    let before = authority(&app).await;
    let replay = upload_with_checkpoint(&mut app, &attempt, Some(unexpected))
        .await
        .unwrap();
    assert!(matches!(
        replay,
        AdminIngestUploadOutcome::Stored {
            deduplicated: true,
            ..
        }
    ));
    assert_eq!(authority(&app).await, before);
    assert_eq!(counts(&app).await, (1, 1, 2));

    let mut rejected = database().await;
    let mut rejection = self::attempt();
    rejection.failure = Some(FailureReason::Malformed);
    rejection.original = None;
    upload_with_checkpoint(&mut rejected, &rejection, Some(unexpected))
        .await
        .unwrap();
    assert_eq!(authority(&rejected).await, ("failed".into(), 0, None));
    assert_eq!(counts(&rejected).await, (1, 0, 1));

    let mut denied = database().await;
    denied
        .batch_execute("UPDATE users SET role = 'member'")
        .await
        .unwrap();
    assert_eq!(
        upload_with_checkpoint(&mut denied, &attempt, Some(unexpected))
            .await
            .unwrap(),
        AdminIngestUploadOutcome::ActorNotAdmin,
    );
    assert_eq!(counts(&denied).await, (0, 0, 0));
}

#[tokio::test]
async fn fresh_upload_rolls_back_all_rows_on_original_checkpoint_or_audit_failure() {
    for refusal in ["original", "checkpoint_error", "checkpoint_zero", "audit"] {
        let mut app = database().await;
        match refusal {
            "original" => app.batch_execute("ALTER TABLE ingest_originals ADD CONSTRAINT deny_original CHECK (FALSE)").await.unwrap(),
            "checkpoint_error" => app.batch_execute("ALTER TABLE ingest_jobs ADD CONSTRAINT deny_checkpoint CHECK (admin_checkpoint IS NULL)").await.unwrap(),
            "checkpoint_zero" => app.batch_execute(
                "CREATE FUNCTION pg_temp.deny_checkpoint() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$;
                 CREATE TRIGGER deny_checkpoint BEFORE UPDATE ON ingest_jobs FOR EACH ROW EXECUTE FUNCTION pg_temp.deny_checkpoint()",
            ).await.unwrap(),
            "audit" => app.batch_execute("ALTER TABLE audit_log ADD CONSTRAINT deny_audit CHECK (FALSE)").await.unwrap(),
            _ => unreachable!(),
        }
        assert!(upload(&mut app, &attempt(), true).await.is_err());
        assert_eq!(counts(&app).await, (0, 0, 0));
    }
}

#[tokio::test]
async fn upload_fences_actors_and_rejected_attempts_never_gain_execution_authority() {
    for state in ["member", "deleting", "absent"] {
        let mut app = database().await;
        match state {
            "member" => app
                .batch_execute("UPDATE users SET role = 'member'")
                .await
                .unwrap(),
            "deleting" => app
                .batch_execute("UPDATE users SET deleting_at = 1")
                .await
                .unwrap(),
            "absent" => app.batch_execute("DELETE FROM users").await.unwrap(),
            _ => unreachable!(),
        }
        let result = upload(&mut app, &attempt(), true).await.unwrap();
        assert_eq!(
            result,
            if state == "deleting" {
                AdminIngestUploadOutcome::AccountDeleting
            } else {
                AdminIngestUploadOutcome::ActorNotAdmin
            }
        );
        assert_eq!(counts(&app).await, (0, 0, 0));
    }
    let mut app = database().await;
    let mut rejected = attempt();
    rejected.failure = Some(FailureReason::Malformed);
    rejected.original = None;
    upload(&mut app, &rejected, true).await.unwrap();
    assert_eq!(authority(&app).await, ("failed".into(), 0, None));
    assert_eq!(counts(&app).await, (1, 0, 1));
}

#[tokio::test]
async fn fresh_upload_refuses_inconsistent_metadata_and_keeps_pre_schema_path_available() {
    for field in [
        "original", "failure", "length", "key", "job", "hash", "wrapped", "kek",
    ] {
        let mut app = database().await;
        let mut bad = attempt();
        match field {
            "original" => bad.original = None,
            "failure" => bad.failure = Some(FailureReason::Malformed),
            "length" => bad.nzb_bytes += 1,
            "key" => bad.idempotency_key.clear(),
            "job" => bad.job_id.clear(),
            "hash" => bad.content_sha256_hex.clear(),
            "wrapped" => bad.original.as_mut().unwrap().dek_wrapped.clear(),
            "kek" => bad.kek_version = 0,
            _ => unreachable!(),
        }
        assert!(upload(&mut app, &bad, true).await.is_err());
        assert_eq!(counts(&app).await, (0, 0, 0));
    }
    let mut app = database().await;
    app.batch_execute("ALTER TABLE ingest_jobs DROP COLUMN admin_checkpoint, DROP COLUMN admin_checkpoint_revision").await.unwrap();
    assert!(upload(&mut app, &attempt(), false).await.is_ok());
    assert_eq!(counts(&app).await, (1, 1, 1));
    // Enabling the prepared path against missing columns fails closed, not a
    // silent legacy fallback. Use a fresh isolated database to reach the insert.
    let mut missing = database().await;
    missing.batch_execute("ALTER TABLE ingest_jobs DROP COLUMN admin_checkpoint, DROP COLUMN admin_checkpoint_revision").await.unwrap();
    assert!(upload(&mut missing, &attempt(), true).await.is_err());
    assert_eq!(counts(&missing).await, (0, 0, 0));
}
