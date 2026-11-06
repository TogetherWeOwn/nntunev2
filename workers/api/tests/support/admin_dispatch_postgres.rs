//! Real private API admission -> posting-disabled dispatch acceptance.
//!
//! Included inside worker_entry only for the postgres-tests library bank: no
//! public store API or runtime entry is added for testing. These tests call the
//! very transaction used by the API store, not a copy of its SQL. The input is
//! a synthetic already-validated attempt, not an HTTP/session/parser exercise.
//! Final tagged bytes/plan and Queue remain bounded fakes; this is neither real
//! storage, enabled preparation/posting, process-crash nor deployed-drain proof.

use super::admin_ingest_upload_on;
use crate::admin_ingest::{self, AdminIngestAttempt, AdminIngestUploadOutcome, FailureReason};
use nntune_ingest::posted_generations::dispatch::{
    pending_file_dispatches, prepare_file_dispatch, recover_file_dispatch, send_file_dispatch,
    DispatchMode, DispatchObject, DispatchObjectReader, DispatchPointer, DispatchQueue,
    DispatchSend, PreparedDispatchFile,
};
use nntune_ingest::posted_generations::{AttemptOutcome, StoreError};
use std::future::Future;
use std::time::Duration;
use tokio_postgres::Client;
use zeroize::Zeroizing;

#[path = "posted_generation_database.rs"]
mod database;
use database::{connect, Quiet};

const MODE: DispatchMode = DispatchMode::PreparationOnly;

#[path = "admin_plan_postgres.rs"]
mod sealed;

fn run(test: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("native admission runtime")
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(20), test)
                .await
                .expect("admin dispatch PostgreSQL test exceeded 20 seconds");
        });
}

fn random_id(prefix: &str) -> String {
    let id = crate::random::secure_random_bytes::<16>().expect("fixture randomness");
    let suffix: String = id.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{prefix}{suffix}")
}

async fn owner(db: &Client) -> String {
    let id = random_id("pg-admit-");
    db.execute(
        "INSERT INTO users (id, email, created_at, role) VALUES ($1, $2, 1700000000, 'admin')",
        &[&id, &format!("{id}@example.invalid")],
    )
    .await
    .quiet("seed admission actor");
    id
}

fn attempt(actor: &str) -> AdminIngestAttempt {
    // Random synthetic payload, never a document with operational content.
    let bytes = crate::random::secure_random_bytes::<79>().expect("fixture payload");
    let hash = admin_ingest::content_sha256_hex(&bytes);
    let job = admin_ingest::derive_job_id(&bytes);
    let key = Zeroizing::new(
        crate::random::secure_random_bytes::<32>().expect("fixture envelope randomness"),
    );
    let original = admin_ingest::encrypt_ingest_original(&key, 1, &hash, &job, &bytes)
        .expect("seal synthetic admitted original");
    AdminIngestAttempt {
        actor_user_id: actor.to_owned(),
        job_id: job,
        idempotency_key: admin_ingest::idempotency_key(&hash),
        content_sha256_hex: hash,
        nzb_bytes: bytes.len() as u64,
        file_count: Some(1),
        segment_count: Some(1),
        failure: None,
        original: Some(original),
        kek_version: 1,
        source_ip: "unknown".to_owned(),
        now: 1_700_000_000,
    }
}

fn stored(
    result: Result<AdminIngestUploadOutcome, crate::handlers::StoreError>,
    replay: bool,
) -> admin_ingest::StoredAdminIngestJob {
    match result {
        Ok(AdminIngestUploadOutcome::Stored { job, deduplicated }) if replay == deduplicated => job,
        _ => panic!("unexpected admission outcome"),
    }
}

// No error path displays rows, envelopes, ids, pointers or tokens.
fn ok<T>(result: Result<T, StoreError>) -> T {
    result.unwrap_or_else(|_| panic!("dispatch adapter error"))
}

async fn counts(db: &Client, input: &AdminIngestAttempt) -> [i64; 5] {
    let row = db
        .query_one(
            "SELECT \
             (SELECT count(*) FROM ingest_jobs WHERE id = $1), \
             (SELECT count(*) FROM ingest_originals WHERE job_id = $1), \
             (SELECT count(*) FROM ingest_file_dispatches WHERE job_id = $1), \
             (SELECT count(*) FROM posted_file_job_attempts WHERE job_id = $1), \
             (SELECT count(*) FROM audit_log WHERE details->>'job_id' = $1)",
            &[&input.job_id],
        )
        .await
        .quiet("count admission effects");
    [row.get(0), row.get(1), row.get(2), row.get(3), row.get(4)]
}

async fn authority(db: &Client, input: &AdminIngestAttempt) -> String {
    db.query_one(
        "SELECT md5(jsonb_build_array( \
         (SELECT to_jsonb(j) FROM ingest_jobs j WHERE j.id = $1), \
         (SELECT to_jsonb(o) FROM ingest_originals o WHERE o.job_id = $1), \
         (SELECT to_jsonb(a) FROM posted_file_job_attempts a WHERE a.job_id = $1), \
         (SELECT jsonb_agg(to_jsonb(r) ORDER BY request_id) \
          FROM posted_file_attempt_receipts r WHERE r.job_id = $1), \
         (SELECT jsonb_agg(to_jsonb(d) ORDER BY id) \
          FROM ingest_file_dispatches d WHERE d.job_id = $1))::text)",
        &[&input.job_id],
    )
    .await
    .quiet("snapshot admission authority")
    .get(0)
}

struct Objects {
    file: DispatchObject,
    plan: DispatchObject,
    reads: usize,
}

impl Objects {
    fn new() -> Self {
        Self {
            file: DispatchObject::new(&random_id("pg-file-"), "version-a")
                .expect("synthetic file reference"),
            plan: DispatchObject::new(&random_id("pg-plan-"), "version-a")
                .expect("synthetic plan reference"),
            reads: 0,
        }
    }

    fn prepared(&self) -> PreparedDispatchFile {
        PreparedDispatchFile::from_bytes(
            0,
            self.file.clone(),
            &[3; 79],
            self.plan.clone(),
            &[5; 47],
        )
        .expect("synthetic final preparation")
    }
}

impl DispatchObjectReader for Objects {
    async fn read(
        &mut self,
        object: &DispatchObject,
        offset: u64,
        limit: usize,
    ) -> Result<Vec<u8>, StoreError> {
        self.reads += 1;
        let (length, byte) = if object == &self.file {
            (79_u64, 3)
        } else if object == &self.plan {
            (47_u64, 5)
        } else {
            return Err(StoreError::InvalidInput);
        };
        Ok(vec![
            byte;
            usize::try_from(length.saturating_sub(offset))
                .unwrap_or(usize::MAX)
                .min(limit)
        ])
    }
}

#[derive(Default)]
struct Queue(Vec<DispatchPointer>);

impl DispatchQueue for Queue {
    async fn send(&mut self, pointer: DispatchPointer) -> Result<(), StoreError> {
        self.0.push(pointer);
        Ok(())
    }
}

async fn prepare(
    db: &mut Client,
    input: &AdminIngestAttempt,
    objects: &Objects,
) -> DispatchPointer {
    ok(prepare_file_dispatch(
        db,
        MODE,
        &input.job_id,
        "prepare-final",
        0,
        &objects.prepared(),
    )
    .await)
    .unwrap_or_else(|| panic!("accepted admission did not prepare"))
}

#[test]
fn postgres_real_admission_dispatch_replays_original_after_client_drop() {
    run(async {
        let mut db = connect().await;
        let actor = owner(&db).await;
        let input = attempt(&actor);
        let admitted = stored(admin_ingest_upload_on(&mut db, &input).await, false);
        assert!(admitted.has_original && admitted.db_status == "queued");
        assert_eq!(counts(&db, &input).await, [1, 1, 0, 0, 1]);
        let mut objects = Objects::new();
        let pointer = prepare(&mut db, &input, &objects).await;
        let before = authority(&db, &input).await;
        assert!(matches!(
            ok(recover_file_dispatch(&mut db, MODE, &pointer, &mut objects).await),
            AttemptOutcome::StaleOrMissing
        ));
        assert!(before == authority(&db, &input).await && objects.reads == 0);
        let mut queue = Queue::default();
        assert_eq!(
            ok(send_file_dispatch(&mut db, MODE, &pointer, &mut objects, &mut queue).await),
            DispatchSend::Acknowledged
        );
        let original = match ok(recover_file_dispatch(&mut db, MODE, &pointer, &mut objects).await)
        {
            AttemptOutcome::Acquired(handle) => handle,
            _ => panic!("started admission did not recover"),
        };
        drop(db);
        let mut db = connect().await;
        let replay = stored(admin_ingest_upload_on(&mut db, &input).await, true);
        assert!(replay.uploader_user_id == actor && replay.db_status == "running");
        assert!(prepare(&mut db, &input, &objects).await == pointer);
        let before = authority(&db, &input).await;
        let recovered = ok(recover_file_dispatch(&mut db, MODE, &pointer, &mut objects).await);
        assert!(matches!(recovered, AttemptOutcome::Acquired(handle) if handle == original));
        assert!(before == authority(&db, &input).await);
        assert_eq!(counts(&db, &input).await, [1, 1, 1, 1, 2]);
    });
}

#[test]
fn postgres_real_admission_rejection_cannot_prepare_or_start() {
    run(async {
        let mut db = connect().await;
        let actor = owner(&db).await;
        let mut input = attempt(&actor);
        input.failure = Some(FailureReason::Malformed);
        input.original = None;
        input.file_count = None;
        input.segment_count = None;
        let job = stored(admin_ingest_upload_on(&mut db, &input).await, false);
        assert!(!job.has_original && job.db_status == "failed");
        let objects = Objects::new();
        let before = authority(&db, &input).await;
        assert!(ok(prepare_file_dispatch(
            &mut db,
            MODE,
            &input.job_id,
            "prepare-final",
            0,
            &objects.prepared()
        )
        .await)
        .is_none());
        assert!(ok(pending_file_dispatches(&db, &input.job_id).await).is_empty());
        assert!(before == authority(&db, &input).await);
        assert_eq!(counts(&db, &input).await, [1, 0, 0, 0, 1]);
    });
}

#[test]
fn postgres_real_admission_demoted_and_deleting_actors_leave_no_dispatchable_job() {
    run(async {
        let mut db = connect().await;
        for deleting in [false, true] {
            let actor = owner(&db).await;
            let input = attempt(&actor);
            db.execute(
                "UPDATE users SET role = CASE WHEN $2 THEN 'admin' ELSE 'member' END, \
                 deleting_at = CASE WHEN $2 THEN to_timestamp(1700000000) ELSE NULL END WHERE id = $1",
                &[&actor, &deleting],
            )
            .await
            .quiet("fence admission actor");
            let outcome = admin_ingest_upload_on(&mut db, &input)
                .await
                .unwrap_or_else(|_| panic!("admission refusal errored"));
            assert!(if deleting {
                outcome == AdminIngestUploadOutcome::AccountDeleting
            } else {
                outcome == AdminIngestUploadOutcome::ActorNotAdmin
            });
            assert_eq!(counts(&db, &input).await, [0; 5]);
            assert!(ok(prepare_file_dispatch(
                &mut db,
                MODE,
                &input.job_id,
                "prepare-final",
                0,
                &Objects::new().prepared()
            )
            .await)
            .is_none());
            assert_eq!(counts(&db, &input).await, [0; 5]);
        }
    });
}

#[test]
fn postgres_real_admission_duplicate_actor_never_rebinds_original_dispatch_owner() {
    run(async {
        let mut db = connect().await;
        let first = owner(&db).await;
        let second = owner(&db).await;
        let mut input = attempt(&first);
        stored(admin_ingest_upload_on(&mut db, &input).await, false);
        let mut objects = Objects::new();
        let pointer = prepare(&mut db, &input, &objects).await;
        let before = authority(&db, &input).await;
        input.actor_user_id = second;
        let duplicate = stored(admin_ingest_upload_on(&mut db, &input).await, true);
        assert!(duplicate.uploader_user_id == first);
        assert!(before == authority(&db, &input).await);
        db.execute("UPDATE users SET role = 'member' WHERE id = $1", &[&first])
            .await
            .quiet("demote original admission owner");
        let mut queue = Queue::default();
        let before = authority(&db, &input).await;
        assert_eq!(
            ok(send_file_dispatch(&mut db, MODE, &pointer, &mut objects, &mut queue).await),
            DispatchSend::StaleOrMissing
        );
        assert!(objects.reads == 0 && queue.0.is_empty());
        assert!(before == authority(&db, &input).await);
        assert_eq!(counts(&db, &input).await, [1, 1, 1, 0, 2]);
    });
}

#[test]
fn postgres_real_admission_audit_error_rolls_back_before_dispatch_and_retry_keeps_identity() {
    run(async {
        let mut db = connect().await;
        let actor = owner(&db).await;
        let input = attempt(&actor);
        let trigger = random_id("pg_reject_");
        db.batch_execute(&format!(
            "CREATE FUNCTION pg_temp.reject_admission() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN RAISE EXCEPTION 'injected admission rollback'; END $$; \
             CREATE TRIGGER {trigger} BEFORE INSERT ON audit_log FOR EACH ROW \
             WHEN (NEW.user_id = '{actor}') EXECUTE FUNCTION pg_temp.reject_admission()"
        ))
        .await
        .quiet("inject admission audit failure");
        assert!(admin_ingest_upload_on(&mut db, &input).await.is_err());
        assert_eq!(counts(&db, &input).await, [0; 5]);
        assert!(ok(prepare_file_dispatch(
            &mut db,
            MODE,
            &input.job_id,
            "prepare-final",
            0,
            &Objects::new().prepared()
        )
        .await)
        .is_none());
        db.batch_execute(&format!("DROP TRIGGER {trigger} ON audit_log"))
            .await
            .quiet("remove admission failure injection");
        let job = stored(admin_ingest_upload_on(&mut db, &input).await, false);
        assert!(job.job_id == input.job_id);
        let objects = Objects::new();
        let pointer = prepare(&mut db, &input, &objects).await;
        assert!(prepare(&mut db, &input, &objects).await == pointer);
        assert_eq!(counts(&db, &input).await, [1, 1, 1, 0, 1]);
    });
}

#[test]
fn postgres_real_admission_concurrent_replay_and_duplicate_send_keep_one_attempt() {
    run(async {
        let mut left = connect().await;
        let mut right = connect().await;
        let actor = owner(&left).await;
        let input = attempt(&actor);
        let (a, b) = tokio::join!(
            admin_ingest_upload_on(&mut left, &input),
            admin_ingest_upload_on(&mut right, &input)
        );
        let (
            Ok(AdminIngestUploadOutcome::Stored {
                deduplicated: a, ..
            }),
            Ok(AdminIngestUploadOutcome::Stored {
                deduplicated: b, ..
            }),
        ) = (a, b)
        else {
            panic!("concurrent admission failed")
        };
        assert!(a != b);
        let mut objects = Objects::new();
        let pointer = prepare(&mut left, &input, &objects).await;
        let mut queue = Queue::default();
        assert_eq!(
            ok(send_file_dispatch(&mut left, MODE, &pointer, &mut objects, &mut queue).await),
            DispatchSend::Acknowledged
        );
        let before = authority(&left, &input).await;
        assert_eq!(
            ok(send_file_dispatch(&mut right, MODE, &pointer, &mut objects, &mut queue).await),
            DispatchSend::Acknowledged
        );
        assert!(queue.0.len() == 2 && queue.0.iter().all(|p| p == &pointer));
        assert!(before == authority(&left, &input).await);
        assert!(ok(pending_file_dispatches(&left, &input.job_id).await).is_empty());
        assert_eq!(counts(&left, &input).await, [1, 1, 1, 1, 2]);
    });
}
