//! Disposable PostgreSQL stage/control/publication-CAS tests. Test seeding is not
//! admission or authenticated control routing; temporary columns do not prove
//! migration checks. Sequential stale-action cases are not concurrent race proof;
//! settlement_races adds two-connection lock-order coverage on shared scratch rows.
#[path = "admin_settlement_races.rs"]
mod settlement_races;
use super::*;
use nntune_ingest::admin_checkpoint_store::{self as store, StageOriginal, StoreError};

fn input(original: &OriginalBody) -> StageOriginal<'_> {
    StageOriginal {
        job_id: original.job_id(),
        idempotency_key: original.idempotency_key(),
        nzb_bytes: original.as_bytes().len(),
        parsed: original.parsed(),
    }
}
use nntune_ingest::{
    admin_checkpoint::{Identity, Snapshot},
    admin_execution::Control,
    admin_gates,
    jobs::JobState,
};

fn identity(original: &OriginalBody) -> Identity<'_> {
    Identity {
        job_id: original.job_id(),
        user_id: "",
        idempotency_key: original.idempotency_key(),
    }
}

async fn seeded() -> (tokio_postgres::Client, OriginalBody, Snapshot) {
    let (app, original, mut cp, _, _) = fixture().await;
    app.batch_execute("ALTER TABLE ingest_jobs ADD COLUMN admin_checkpoint JSONB, ADD COLUMN admin_checkpoint_revision BIGINT NOT NULL DEFAULT 0").await.unwrap();
    cp.state = JobState::Queued;
    cp.admin_release_mbid = None;
    cp.probe_tally = None;
    let snapshot = Snapshot::initial(cp, &identity(&original)).unwrap();
    seed(&app, &snapshot).await;
    (app, original, snapshot)
}

// Explicit fixture setup only; production must initialize in upload's original
// transaction and apply controls under the same fenced authority, never this SQL.
async fn seed(app: &tokio_postgres::Client, snapshot: &Snapshot) {
    app.execute(
        "UPDATE ingest_jobs SET admin_checkpoint = $1::text::jsonb, admin_checkpoint_revision = $2, status = $3",
        &[&snapshot.encode().unwrap(), &snapshot.revision(), &snapshot.status()],
    ).await.unwrap();
}

async fn publication_fixture() -> (
    tokio_postgres::Client,
    OriginalBody,
    Snapshot,
    Vec<MbRecording>,
    Vec<FileGeometry>,
) {
    let (mut app, original, mut cp, recordings, geometry) = fixture().await;
    app.batch_execute("ALTER TABLE ingest_jobs ADD COLUMN admin_checkpoint JSONB, ADD COLUMN admin_checkpoint_revision BIGINT NOT NULL DEFAULT 0").await.unwrap();
    cp.state = JobState::Queued;
    cp.admin_release_mbid = None;
    cp.probe_tally = None;
    let snapshot = Snapshot::initial(cp.clone(), &identity(&original)).unwrap();
    seed(&app, &snapshot).await;
    let mut runtime = DatabaseRuntime {
        app: &mut app,
        original: &original,
        snapshot,
        services: Runtime(cp.clone()),
    };
    for _ in 0..4 {
        run_stage(
            &mut runtime,
            &mut cp,
            original.job_id(),
            original.idempotency_key(),
            original.parsed(),
        )
        .await
        .unwrap();
    }
    assert_eq!(cp.state, JobState::Writing);
    let snapshot = runtime.snapshot;
    (app, original, snapshot, recordings, geometry)
}

struct DatabaseRuntime<'a> {
    app: &'a mut tokio_postgres::Client,
    original: &'a OriginalBody,
    snapshot: Snapshot,
    services: Runtime,
}
impl StageRuntime for DatabaseRuntime<'_> {
    async fn match_release(&mut self, parsed: &Document) -> Result<Option<String>, StageError> {
        self.services.match_release(parsed).await
    }
    async fn probe(&mut self, request: &ProbeRequest) -> Result<ProbeOutcome, StageError> {
        self.services.probe(request).await
    }
    async fn persist(
        &mut self,
        expected: &JobCheckpoint,
        next: &JobCheckpoint,
    ) -> Result<bool, StageError> {
        if expected != self.snapshot.checkpoint() {
            return Ok(false);
        }
        match store::persist_stage(self.app, &input(self.original), &self.snapshot, next).await {
            Ok(snapshot) => {
                self.snapshot = snapshot;
                Ok(true)
            }
            Err(StoreError::Stale) => Ok(false),
            Err(_) => Err(StageError::Store),
        }
    }
}

#[derive(Default)]
struct DeliveryQueue {
    messages: Vec<String>,
    calls: usize,
    reject: bool,
    uncertain: bool,
    pending: bool,
}

impl store::DeliverySender for DeliveryQueue {
    type Error = StoreError;

    async fn send(&mut self, encoded: String) -> Result<(), Self::Error> {
        self.calls += 1;
        if self.reject {
            return Err(StoreError::Database);
        }
        self.messages.push(encoded);
        if self.pending {
            return std::future::pending().await;
        }
        if self.uncertain {
            return Err(StoreError::Database);
        }
        Ok(())
    }
}

#[tokio::test]
async fn delivery_recovers_lost_and_uncertain_sends_from_committed_progress_without_reset() {
    let (mut app, original, initial) = seeded().await;
    let before = authority_row(&app).await;
    let mut queue = DeliveryQueue {
        reject: true,
        ..DeliveryQueue::default()
    };
    assert_eq!(
        store::enqueue_delivery(&app, &input(&original), 1, &mut queue).await,
        Err(store::DeliveryError::Send)
    );
    assert!(queue.messages.is_empty());
    assert_eq!(authority_row(&app).await, before);
    queue.reject = false;
    queue.uncertain = true;
    assert_eq!(
        store::enqueue_delivery(&app, &input(&original), 1, &mut queue).await,
        Err(store::DeliveryError::Send)
    );
    queue.uncertain = false;
    assert_eq!(
        store::enqueue_delivery(&app, &input(&original), 1, &mut queue).await,
        Ok(true)
    );
    assert_eq!(queue.messages.len(), 2);
    assert_eq!(queue.messages[0], queue.messages[1]);
    let hint = store::Delivery::decode(&queue.messages[0]).unwrap();
    assert_eq!(
        (hint.job_id(), hint.idempotency_key(), hint.generation()),
        (original.job_id(), original.idempotency_key(), 1)
    );
    assert_eq!(authority_row(&app).await, before);
    let mut snapshot = initial;
    for _ in 0..4 {
        // Simulate loss of each new continuation. Even the first delivery must
        // reload persisted progress, never trust its original stage/attempt.
        let loaded = store::load_delivery(&app, &input(&original), &hint)
            .await
            .unwrap()
            .unwrap();
        assert!(loaded == snapshot);
        let mut cp = loaded.checkpoint().clone();
        let mut runtime = DatabaseRuntime {
            app: &mut app,
            original: &original,
            snapshot: loaded,
            services: Runtime(cp.clone()),
        };
        run_stage(
            &mut runtime,
            &mut cp,
            original.job_id(),
            original.idempotency_key(),
            original.parsed(),
        )
        .await
        .unwrap();
        snapshot = runtime.snapshot;
        assert_eq!(
            store::enqueue_delivery(&app, &input(&original), 1, &mut queue).await,
            Ok(true)
        );
        let continuation = store::Delivery::decode(queue.messages.last().unwrap()).unwrap();
        assert!(continuation == hint);
        assert!(
            store::load_delivery(&app, &input(&original), &continuation)
                .await
                .unwrap()
                .unwrap()
                == snapshot
        );
    }
    assert_eq!(
        (
            snapshot.checkpoint().state,
            snapshot.revision(),
            snapshot.generation()
        ),
        (JobState::Writing, 5, 1)
    );
    assert_eq!(counts(&app).await, (0, 0, 0));
}

#[tokio::test]
async fn delivery_send_cancellation_and_manual_controls_never_adopt_a_newer_generation() {
    let (mut app, original, initial) = seeded().await;
    let before = authority_row(&app).await;
    let mut queue = DeliveryQueue {
        pending: true,
        ..DeliveryQueue::default()
    };
    assert!(tokio::time::timeout(
        std::time::Duration::from_secs(1),
        store::enqueue_delivery(&app, &input(&original), 1, &mut queue)
    )
    .await
    .is_err());
    assert_eq!(
        queue.messages.len(),
        1,
        "send accepted before its response was cancelled"
    );
    assert_eq!(authority_row(&app).await, before);
    queue.pending = false;
    let old = store::Delivery::decode(&queue.messages[0]).unwrap();
    let mut cp = initial.checkpoint().clone();
    cp.state = JobState::NeedsReview;
    let parked = initial.successor(cp, false, &identity(&original)).unwrap();
    seed(&app, &parked).await;
    assert!(store::load_delivery(&app, &input(&original), &old)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        store::enqueue_delivery(&app, &input(&original), 1, &mut queue).await,
        Ok(false)
    );
    assert_eq!(queue.calls, 1);
    let restarted =
        store::apply_control(&mut app, &input(&original), &parked, Control::RetryReview)
            .await
            .unwrap();
    assert_eq!(
        store::enqueue_delivery(&app, &input(&original), 1, &mut queue).await,
        Err(store::DeliveryError::Authority(StoreError::Stale))
    );
    assert_eq!(
        store::load_delivery(&app, &input(&original), &old)
            .await
            .err(),
        Some(StoreError::Stale)
    );
    assert_eq!(queue.calls, 1);
    assert_eq!(
        store::enqueue_delivery(&app, &input(&original), restarted.generation(), &mut queue).await,
        Ok(true)
    );
    let new = store::Delivery::decode(queue.messages.last().unwrap()).unwrap();
    assert_eq!(new.generation(), restarted.generation());
    assert!(
        store::load_delivery(&app, &input(&original), &new)
            .await
            .unwrap()
            .unwrap()
            == restarted
    );
    let rejected = store::apply_control(&mut app, &input(&original), &restarted, Control::Reject)
        .await
        .unwrap();
    assert_eq!(
        store::load_delivery(&app, &input(&original), &new)
            .await
            .err(),
        Some(StoreError::Stale)
    );
    assert_eq!(
        store::enqueue_delivery(&app, &input(&original), new.generation(), &mut queue).await,
        Err(store::DeliveryError::Authority(StoreError::Stale))
    );
    assert_eq!(
        store::enqueue_delivery(&app, &input(&original), rejected.generation(), &mut queue).await,
        Ok(false)
    );
    assert_eq!(queue.calls, 2);
    assert!(
        store::load(&app, &input(&original), rejected.generation())
            .await
            .unwrap()
            == rejected
    );
}

#[tokio::test]
async fn delivery_refuses_missing_malformed_or_unlinked_authority_before_sending() {
    let (app, original, _) = seeded().await;
    let mut queue = DeliveryQueue::default();
    let hint = store::prepare_delivery(&app, &input(&original), 1)
        .await
        .unwrap()
        .unwrap();
    let encoded = hint.encode().unwrap();
    for field in ["job_id", "idempotency_key"] {
        let mut value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        value[field] = if field == "job_id" {
            "other_job".into()
        } else {
            format!("admin:{}", "0".repeat(64)).into()
        };
        let foreign = store::Delivery::decode(&value.to_string()).unwrap();
        assert_eq!(
            store::load_delivery(&app, &input(&original), &foreign)
                .await
                .err(),
            Some(StoreError::Invalid)
        );
    }
    for generation in [0, -1, 2] {
        let expected = if generation <= 0 {
            StoreError::Invalid
        } else {
            StoreError::Stale
        };
        assert_eq!(
            store::enqueue_delivery(&app, &input(&original), generation, &mut queue).await,
            Err(store::DeliveryError::Authority(expected))
        );
    }
    for (change, restore, expected) in [
        (
            "UPDATE ingest_jobs SET admin_checkpoint = NULL",
            "",
            StoreError::Invalid,
        ),
        (
            "UPDATE ingest_jobs SET admin_checkpoint = '{}'::jsonb",
            "",
            StoreError::Invalid,
        ),
        (
            "UPDATE ingest_jobs SET status = 'running'",
            "UPDATE ingest_jobs SET status = 'queued'",
            StoreError::Invalid,
        ),
        (
            "UPDATE ingest_jobs SET source = 'upload'",
            "UPDATE ingest_jobs SET source = 'admin'",
            StoreError::Stale,
        ),
        (
            "UPDATE ingest_originals SET nzb_bytes = nzb_bytes + 1",
            "UPDATE ingest_originals SET nzb_bytes = nzb_bytes - 1",
            StoreError::Stale,
        ),
    ] {
        let saved = authority_row(&app).await;
        app.batch_execute(change).await.unwrap();
        let before = authority_row(&app).await;
        assert_eq!(
            store::enqueue_delivery(&app, &input(&original), 1, &mut queue).await,
            Err(store::DeliveryError::Authority(expected))
        );
        assert_eq!(
            store::load_delivery(&app, &input(&original), &hint)
                .await
                .err(),
            Some(expected)
        );
        assert_eq!(authority_row(&app).await, before);
        if restore.is_empty() {
            app.execute(
                "UPDATE ingest_jobs SET admin_checkpoint = $1::text::jsonb",
                &[&saved.2],
            )
            .await
            .unwrap();
        } else {
            app.batch_execute(restore).await.unwrap();
        }
    }
    app.batch_execute("DELETE FROM ingest_originals")
        .await
        .unwrap();
    assert_eq!(
        store::enqueue_delivery(&app, &input(&original), 1, &mut queue).await,
        Err(store::DeliveryError::Authority(StoreError::Stale))
    );
    assert_eq!(queue.calls, 0);
    assert!(queue.messages.is_empty());
}

#[tokio::test]
async fn delivery_replay_after_atomic_publication_does_not_enqueue_or_publish_again() {
    let (mut app, original, writing, recordings, geometry) = publication_fixture().await;
    let hint = store::prepare_delivery(&app, &input(&original), writing.generation())
        .await
        .unwrap()
        .unwrap();
    let mut sealer = Sealer::default();
    let done = store::publish(
        &mut app,
        &input(&original),
        &writing,
        &recordings,
        &geometry,
        &mut sealer,
    )
    .await
    .unwrap();
    assert!(store::load_delivery(&app, &input(&original), &hint)
        .await
        .unwrap()
        .is_none());
    let mut queue = DeliveryQueue::default();
    assert_eq!(
        store::enqueue_delivery(&app, &input(&original), hint.generation(), &mut queue).await,
        Ok(false)
    );
    assert_eq!((queue.calls, sealer.calls), (0, 1));
    assert_eq!(counts(&app).await, (1, 1, 1));
    assert!(
        store::load(&app, &input(&original), hint.generation())
            .await
            .unwrap()
            == done
    );
}

async fn authority_row(app: &tokio_postgres::Client) -> (String, i64, Option<String>) {
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
async fn explicit_null_reconciliation_commits_fresh_queued_authority_without_reset_or_replay() {
    let (mut app, original, _) = seeded().await;
    for (status, revision) in [("queued", 0_i64), ("running", 7)] {
        // Historical sentinel setup, never an automatic production reset.
        app.execute("UPDATE ingest_jobs SET status = $1, admin_checkpoint = NULL, admin_checkpoint_revision = $2", &[&status, &revision]).await.unwrap();
        assert_eq!(
            store::load(&app, &input(&original), 1).await.err(),
            Some(StoreError::Invalid)
        );
        let reconciled = store::reconcile_null(&mut app, &input(&original), status, revision)
            .await
            .unwrap();
        assert_eq!(
            (
                reconciled.revision(),
                reconciled.generation(),
                reconciled.status()
            ),
            (revision + 1, revision + 1, "queued")
        );
        let cp = reconciled.checkpoint();
        assert_eq!((cp.state, cp.attempt), (JobState::Queued, 1));
        assert!(cp.admin_release_mbid.is_none() && cp.probe_tally.is_none());
        assert!(cp.failure_code.is_none() && cp.failure_reason.is_none());
        assert!(
            store::load(&app, &input(&original), revision + 1)
                .await
                .unwrap()
                == reconciled
        );
        if revision > 0 {
            assert_eq!(
                store::load(&app, &input(&original), 1).await.err(),
                Some(StoreError::Stale)
            );
        }
        assert_eq!(
            store::reconcile_null(&mut app, &input(&original), status, revision)
                .await
                .err(),
            Some(StoreError::Stale)
        );
        assert!(
            store::load(&app, &input(&original), revision + 1)
                .await
                .unwrap()
                == reconciled
        );
        let mut next = cp.clone();
        next.state = JobState::Parsing;
        let parsed = store::persist_stage(&mut app, &input(&original), &reconciled, &next)
            .await
            .unwrap();
        assert_eq!(
            (parsed.revision(), parsed.generation(), parsed.status()),
            (revision + 2, revision + 1, "running")
        );
    }
}

#[tokio::test]
async fn null_reconciliation_refuses_populated_authority_and_parked_or_terminal_jobs() {
    let (mut app, original, initial) = seeded().await;
    let oversized = serde_json::json!({"extra": "a".repeat(16_384)}).to_string();
    for encoded in [
        initial.encode().unwrap(),
        "{}".into(),
        "null".into(),
        oversized,
    ] {
        app.execute("UPDATE ingest_jobs SET status = 'queued', admin_checkpoint = $1::text::jsonb, admin_checkpoint_revision = 0", &[&encoded]).await.unwrap();
        let before = authority_row(&app).await;
        assert_eq!(
            store::reconcile_null(&mut app, &input(&original), "queued", 0)
                .await
                .err(),
            Some(StoreError::Stale)
        );
        assert_eq!(authority_row(&app).await, before);
    }
    for status in [
        "needs_review",
        "needs_archive_read",
        "rejected",
        "done",
        "failed",
        "other",
    ] {
        app.execute("UPDATE ingest_jobs SET status = $1, admin_checkpoint = NULL, admin_checkpoint_revision = 0", &[&status]).await.unwrap();
        let before = authority_row(&app).await;
        // Lying about the expected eligible status must not bypass the row check.
        assert_eq!(
            store::reconcile_null(&mut app, &input(&original), "queued", 0)
                .await
                .err(),
            Some(StoreError::Stale)
        );
        assert_eq!(
            store::reconcile_null(&mut app, &input(&original), status, 0)
                .await
                .err(),
            Some(StoreError::Invalid)
        );
        assert_eq!(authority_row(&app).await, before);
    }
}

#[tokio::test]
async fn null_reconciliation_rechecks_expected_status_revision_and_original_linkage() {
    let (mut app, original, _) = seeded().await;
    app.batch_execute("UPDATE ingest_jobs SET status = 'running', admin_checkpoint = NULL, admin_checkpoint_revision = 7").await.unwrap();
    let before = authority_row(&app).await;
    for (status, revision) in [("queued", 7), ("running", 6), ("running", 8)] {
        assert_eq!(
            store::reconcile_null(&mut app, &input(&original), status, revision)
                .await
                .err(),
            Some(StoreError::Stale)
        );
        assert_eq!(authority_row(&app).await, before);
    }
    for (change, restore) in [
        ("UPDATE ingest_jobs SET source = 'upload'", "UPDATE ingest_jobs SET source = 'admin'"),
        ("UPDATE ingest_jobs SET idempotency_key = idempotency_key || 'a'", "UPDATE ingest_jobs SET idempotency_key = left(idempotency_key, length(idempotency_key) - 1)"),
        ("UPDATE ingest_jobs SET content_sha256 = content_sha256 || 'a'", "UPDATE ingest_jobs SET content_sha256 = left(content_sha256, length(content_sha256) - 1)"),
        ("UPDATE ingest_originals SET nzb_bytes = nzb_bytes + 1", "UPDATE ingest_originals SET nzb_bytes = nzb_bytes - 1"),
        ("UPDATE ingest_originals SET content_sha256 = content_sha256 || 'a'", "UPDATE ingest_originals SET content_sha256 = left(content_sha256, length(content_sha256) - 1)"),
        ("UPDATE ingest_originals SET nzb_ref_envelope_version = 1", "UPDATE ingest_originals SET nzb_ref_envelope_version = 2"),
        ("UPDATE ingest_originals SET nzb_ref_encrypted = nzb_ref_encrypted || decode('00','hex')", "UPDATE ingest_originals SET nzb_ref_encrypted = substring(nzb_ref_encrypted for octet_length(nzb_ref_encrypted) - 1)"),
    ] {
        app.batch_execute(change).await.unwrap();
        assert_eq!(store::reconcile_null(&mut app, &input(&original), "running", 7).await.err(), Some(StoreError::Stale));
        assert_eq!(authority_row(&app).await, before);
        app.batch_execute(restore).await.unwrap();
    }
    app.batch_execute("DELETE FROM ingest_originals")
        .await
        .unwrap();
    assert_eq!(
        store::reconcile_null(&mut app, &input(&original), "running", 7)
            .await
            .err(),
        Some(StoreError::Stale)
    );
    assert_eq!(authority_row(&app).await, before);
}

#[tokio::test]
async fn null_reconciliation_sql_failure_zero_row_and_overflow_leave_legacy_authority_untouched() {
    let (mut app, original, _) = seeded().await;
    app.batch_execute("UPDATE ingest_jobs SET status = 'running', admin_checkpoint = NULL, admin_checkpoint_revision = 7").await.unwrap();
    let before = authority_row(&app).await;
    for (function, error) in [
        ("CREATE OR REPLACE FUNCTION pg_temp.refuse_reconciliation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'refused'; END $$", StoreError::Database),
        ("CREATE OR REPLACE FUNCTION pg_temp.refuse_reconciliation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$", StoreError::Stale),
    ] {
        app.batch_execute(function).await.unwrap();
        app.batch_execute("CREATE TRIGGER refuse_reconciliation BEFORE UPDATE ON ingest_jobs FOR EACH ROW EXECUTE FUNCTION pg_temp.refuse_reconciliation()").await.unwrap();
        assert_eq!(store::reconcile_null(&mut app, &input(&original), "running", 7).await.err(), Some(error));
        assert_eq!(authority_row(&app).await, before);
        app.batch_execute("DROP TRIGGER refuse_reconciliation ON ingest_jobs").await.unwrap();
    }
    for revision in [-1_i64, i64::MAX] {
        app.execute(
            "UPDATE ingest_jobs SET admin_checkpoint_revision = $1",
            &[&revision],
        )
        .await
        .unwrap();
        let before = authority_row(&app).await;
        assert_eq!(
            store::reconcile_null(&mut app, &input(&original), "running", revision)
                .await
                .err(),
            Some(StoreError::Invalid)
        );
        assert_eq!(authority_row(&app).await, before);
    }
}

#[tokio::test]
async fn original_reads_preserve_fenced_state_and_do_not_reconcile_legacy_authority() {
    let (app, original, initial) = seeded().await;
    for state in [
        JobState::Queued,
        JobState::Parsing,
        JobState::NeedsReview,
        JobState::NeedsArchiveRead,
    ] {
        let mut cp = initial.checkpoint().clone();
        cp.state = state;
        let expected = initial.successor(cp, true, &identity(&original)).unwrap();
        seed(&app, &expected).await;
        for _ in 0..2 {
            let reopened = read_original(
                &Authority,
                &mut PostgresOriginalRepository { client: &app },
                &Keys,
                original.job_id(),
            )
            .await
            .unwrap();
            assert!(reopened.as_bytes() == original.as_bytes());
            assert!(
                store::load(&app, &input(&reopened), expected.generation())
                    .await
                    .unwrap()
                    == expected
            );
            assert_eq!(
                store::load(&app, &input(&reopened), initial.generation())
                    .await
                    .err(),
                Some(StoreError::Stale)
            );
        }
    }
    // Reading/decrypting cannot repair a null or malformed old checkpoint or
    // manufacture execution authority. Reconciliation is a separate control.
    for encoded in [None, Some("{}"), Some("null")] {
        app.execute("UPDATE ingest_jobs SET status = 'queued', admin_checkpoint = $1::text::jsonb, admin_checkpoint_revision = 0", &[&encoded]).await.unwrap();
        let reopened = read_original(
            &Authority,
            &mut PostgresOriginalRepository { client: &app },
            &Keys,
            original.job_id(),
        )
        .await
        .unwrap();
        assert_eq!(
            store::load(&app, &input(&reopened), 1).await.err(),
            Some(StoreError::Invalid)
        );
        let row = app
            .query_one(
                "SELECT status, admin_checkpoint::text, admin_checkpoint_revision FROM ingest_jobs",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(row.get::<_, String>(0), "queued");
        assert_eq!(row.get::<_, Option<String>>(1).as_deref(), encoded);
        assert_eq!(row.get::<_, i64>(2), 0);
    }
}

#[tokio::test]
async fn legacy_user_terminal_sql_cannot_mutate_or_converge_admin_authority() {
    use nntune_ingest::ingest_jobs::{
        classify_transition_zero_rows, select_ingest_job_status_sql, transition_ingest_job_sql,
        TransitionOutcome,
    };
    let (app, original, initial, _, _) = publication_fixture().await;
    app.batch_execute("CREATE TEMP TABLE users (id TEXT PRIMARY KEY, deleting_at BIGINT); INSERT INTO users VALUES ('uploader', NULL); ALTER TABLE ingest_jobs ADD COLUMN user_id TEXT; UPDATE ingest_jobs SET user_id = 'uploader'").await.unwrap();
    for state in [
        JobState::Queued,
        JobState::Parsing,
        JobState::NeedsReview,
        JobState::NeedsArchiveRead,
        JobState::Rejected,
        JobState::Done,
        JobState::Failed,
    ] {
        let mut cp = initial.checkpoint().clone();
        cp.state = state;
        if matches!(state, JobState::Queued | JobState::Parsing) {
            cp.admin_release_mbid = None;
            cp.probe_tally = None;
        }
        let expected = initial.successor(cp, true, &identity(&original)).unwrap();
        seed(&app, &expected).await;
        for terminal in ["done", "failed"] {
            assert_eq!(
                app.execute(
                    transition_ingest_job_sql(),
                    &[&original.job_id(), &"uploader", &terminal]
                )
                .await
                .unwrap(),
                0
            );
            let rows = app
                .query(
                    select_ingest_job_status_sql(),
                    &[&original.job_id(), &"uploader"],
                )
                .await
                .unwrap();
            assert!(rows.is_empty());
            assert_eq!(
                classify_transition_zero_rows(None),
                TransitionOutcome::MissingOrFenced
            );
            assert!(
                store::load(&app, &input(&original), expected.generation())
                    .await
                    .unwrap()
                    == expected
            );
        }
    }
    // Non-admin terminal writes still honor the existing owner/deletion fence.
    app.batch_execute("UPDATE ingest_jobs SET source = 'pipeline', status = 'queued'")
        .await
        .unwrap();
    assert_eq!(
        app.execute(
            transition_ingest_job_sql(),
            &[&original.job_id(), &"other", &"done"]
        )
        .await
        .unwrap(),
        0
    );
    app.batch_execute("UPDATE users SET deleting_at = 1")
        .await
        .unwrap();
    assert_eq!(
        app.execute(
            transition_ingest_job_sql(),
            &[&original.job_id(), &"uploader", &"done"]
        )
        .await
        .unwrap(),
        0
    );
    app.batch_execute("UPDATE users SET deleting_at = NULL")
        .await
        .unwrap();
    assert_eq!(
        app.execute(
            transition_ingest_job_sql(),
            &[&original.job_id(), &"uploader", &"done"]
        )
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        app.execute(
            transition_ingest_job_sql(),
            &[&original.job_id(), &"uploader", &"failed"]
        )
        .await
        .unwrap(),
        0
    );
    let row = app
        .query_one(
            select_ingest_job_status_sql(),
            &[&original.job_id(), &"uploader"],
        )
        .await
        .unwrap();
    assert_eq!(
        classify_transition_zero_rows(Some(row.get::<_, &str>(0))),
        TransitionOutcome::Converged
    );
}

#[tokio::test]
async fn real_stage_store_runs_to_writing_and_refuses_publication_and_duplicate_results() {
    let (mut app, original, snapshot) = seeded().await;
    let mut cp = snapshot.checkpoint().clone();
    let mut runtime = DatabaseRuntime {
        app: &mut app,
        original: &original,
        snapshot,
        services: Runtime(cp.clone()),
    };
    for revision in 2..=5 {
        let expected = runtime.snapshot.clone();
        run_stage(
            &mut runtime,
            &mut cp,
            original.job_id(),
            original.idempotency_key(),
            original.parsed(),
        )
        .await
        .unwrap();
        assert_eq!(
            (runtime.snapshot.revision(), runtime.snapshot.generation()),
            (revision, 1)
        );
        let loaded = store::load(runtime.app, &input(&original), 1)
            .await
            .unwrap();
        assert!(loaded == runtime.snapshot);
        assert_eq!(
            store::persist_stage(runtime.app, &input(&original), &expected, &cp)
                .await
                .err(),
            Some(StoreError::Stale)
        );
        runtime.snapshot = loaded;
    }
    assert_eq!(cp.state, JobState::Writing);
    let mut done = cp.clone();
    nntune_ingest::jobs::advance_checkpoint(&mut done, false);
    assert_eq!(done.state, JobState::Done);
    assert_eq!(
        store::persist_stage(runtime.app, &input(&original), &runtime.snapshot, &done)
            .await
            .err(),
        Some(StoreError::Invalid)
    );
    assert!(
        store::load(runtime.app, &input(&original), 1)
            .await
            .unwrap()
            == runtime.snapshot
    );
    assert_eq!(counts(runtime.app).await, (0, 0, 0));
}

#[tokio::test]
async fn store_compares_full_checkpoint_revision_and_captured_generation() {
    let (mut app, original, initial) = seeded().await;
    let mut next = initial.checkpoint().clone();
    next.state = JobState::Parsing;
    // Same revision/generation/status/identity, different whole checkpoint.
    let mut changed: serde_json::Value = serde_json::from_str(&initial.encode().unwrap()).unwrap();
    changed["checkpoint"]["failure_code"] = "parse_failed".into();
    app.execute(
        "UPDATE ingest_jobs SET admin_checkpoint = $1::text::jsonb",
        &[&changed.to_string()],
    )
    .await
    .unwrap();
    assert_eq!(
        store::persist_stage(&mut app, &input(&original), &initial, &next)
            .await
            .err(),
        Some(StoreError::Stale)
    );
    seed(&app, &initial).await;
    // ABA of checkpoint contents at another revision must still lose.
    let repeated = initial
        .successor(initial.checkpoint().clone(), false, &identity(&original))
        .unwrap();
    seed(&app, &repeated).await;
    assert_eq!(
        store::persist_stage(&mut app, &input(&original), &initial, &next)
            .await
            .err(),
        Some(StoreError::Stale)
    );
    // Simulate a legal manual retry after parking: same eventual match/probe
    // contents, but old queue work must not adopt the new generation.
    let mut cp = initial.checkpoint().clone();
    cp.state = JobState::NeedsReview;
    let parked = repeated
        .successor(cp.clone(), false, &identity(&original))
        .unwrap();
    assert!(admin_gates::retry_review(&mut cp));
    let retried = parked
        .successor(cp.clone(), true, &identity(&original))
        .unwrap();
    seed(&app, &retried).await;
    assert_eq!(
        store::load(&app, &input(&original), 1).await.err(),
        Some(StoreError::Stale)
    );
    assert!(
        store::load(&app, &input(&original), retried.generation())
            .await
            .unwrap()
            == retried
    );
    assert_eq!(
        store::persist_stage(&mut app, &input(&original), &initial, &next)
            .await
            .err(),
        Some(StoreError::Stale)
    );
    // Public Snapshot helpers cannot grant an empty/non-row owner identity.
    let mut foreign = initial.checkpoint().clone();
    foreign.user_id = "stage_user".into();
    let foreign = Snapshot::initial(
        foreign,
        &Identity {
            user_id: "stage_user",
            ..identity(&original)
        },
    )
    .unwrap();
    assert_eq!(
        store::persist_stage(&mut app, &input(&original), &foreign, &next)
            .await
            .err(),
        Some(StoreError::Invalid)
    );
}

#[tokio::test]
async fn store_refuses_null_malformed_oversized_status_mismatched_and_overflowed_authority() {
    let (mut app, original, initial) = seeded().await;
    let text = initial.encode().unwrap();
    for bad in [
        "null".to_owned(),
        "{}".to_owned(),
        text.replace("\"schema_version\":1", "\"schema_version\":2"),
        text.replace("\"schema_version\":1", "\"extra\":0,\"schema_version\":1"),
        serde_json::json!({ "extra": "x".repeat(16384) }).to_string(),
    ] {
        app.execute(
            "UPDATE ingest_jobs SET admin_checkpoint = $1::text::jsonb",
            &[&bad],
        )
        .await
        .unwrap();
        assert_eq!(
            store::load(&app, &input(&original), 1).await.err(),
            Some(StoreError::Invalid)
        );
    }
    app.batch_execute("UPDATE ingest_jobs SET admin_checkpoint = NULL")
        .await
        .unwrap();
    assert_eq!(
        store::load(&app, &input(&original), 1).await.err(),
        Some(StoreError::Invalid)
    );
    seed(&app, &initial).await;
    app.batch_execute("UPDATE ingest_jobs SET status = 'running'")
        .await
        .unwrap();
    assert_eq!(
        store::load(&app, &input(&original), 1).await.err(),
        Some(StoreError::Invalid)
    );
    seed(&app, &initial).await;
    for revision in [0i64, -1] {
        app.execute(
            "UPDATE ingest_jobs SET admin_checkpoint_revision = $1",
            &[&revision],
        )
        .await
        .unwrap();
        assert_eq!(
            store::load(&app, &input(&original), 1).await.err(),
            Some(StoreError::Invalid)
        );
    }
    app.execute(
        "UPDATE ingest_jobs SET admin_checkpoint_revision = $1",
        &[&i64::MAX],
    )
    .await
    .unwrap();
    let at_limit = store::load(&app, &input(&original), 1).await.unwrap();
    let mut next = at_limit.checkpoint().clone();
    next.state = JobState::Parsing;
    assert_eq!(
        store::persist_stage(&mut app, &input(&original), &at_limit, &next)
            .await
            .err(),
        Some(StoreError::Invalid)
    );
    assert!(store::load(&app, &input(&original), 1).await.unwrap() == at_limit);
}

#[tokio::test]
async fn store_rechecks_source_content_key_original_link_and_byte_count_before_cas() {
    let (mut app, original, initial) = seeded().await;
    let mut next = initial.checkpoint().clone();
    next.state = JobState::Parsing;
    let hash = intake::content_sha256_hex(original.as_bytes());
    let bytes = original.as_bytes().len() as i64;
    for mutate in [
        "UPDATE ingest_jobs SET source = 'upload'",
        "UPDATE ingest_jobs SET content_sha256 = repeat('0',64)",
        "UPDATE ingest_jobs SET idempotency_key = 'admin:' || repeat('0',64)",
        "UPDATE ingest_jobs SET nzb_bytes = nzb_bytes + 1",
        "UPDATE ingest_originals SET content_sha256 = repeat('0',64)",
        "UPDATE ingest_originals SET nzb_bytes = nzb_bytes + 1",
        "UPDATE ingest_originals SET nzb_ref_envelope_version = 1",
    ] {
        app.batch_execute(mutate).await.unwrap();
        assert_eq!(
            store::load(&app, &input(&original), 1).await.err(),
            Some(StoreError::Stale)
        );
        assert_eq!(
            store::persist_stage(&mut app, &input(&original), &initial, &next)
                .await
                .err(),
            Some(StoreError::Stale)
        );
        app.execute("UPDATE ingest_jobs SET source = 'admin', content_sha256 = $1, idempotency_key = 'admin:' || $1, nzb_bytes = $2", &[&hash, &bytes]).await.unwrap();
        app.execute("UPDATE ingest_originals SET content_sha256 = $1, nzb_bytes = $2, nzb_ref_envelope_version = 2", &[&hash, &bytes]).await.unwrap();
    }
    assert!(store::load(&app, &input(&original), 1).await.unwrap() == initial);
}

#[tokio::test]
async fn store_accepts_semantic_optional_null_and_never_restarts_parked_or_terminal_rows() {
    let (mut app, original, initial) = seeded().await;
    app.batch_execute("UPDATE ingest_jobs SET admin_checkpoint = jsonb_set(admin_checkpoint, '{checkpoint,admin_release_mbid}', 'null'::jsonb)").await.unwrap();
    let loaded = store::load(&app, &input(&original), 1).await.unwrap();
    assert!(loaded == initial);
    let mut next = loaded.checkpoint().clone();
    next.state = JobState::Parsing;
    let saved = store::persist_stage(&mut app, &input(&original), &loaded, &next)
        .await
        .unwrap();
    for state in [
        JobState::NeedsReview,
        JobState::NeedsArchiveRead,
        JobState::Rejected,
        JobState::Failed,
    ] {
        let mut cp = saved.checkpoint().clone();
        cp.state = state;
        let settled = saved.successor(cp, false, &identity(&original)).unwrap();
        seed(&app, &settled).await;
        let loaded = store::load(&app, &input(&original), 1).await.unwrap();
        assert!(loaded == settled);
        assert_eq!(
            store::persist_stage(&mut app, &input(&original), &loaded, &next)
                .await
                .err(),
            Some(StoreError::Invalid)
        );
        assert!(store::load(&app, &input(&original), 1).await.unwrap() == settled);
    }
}

#[tokio::test]
async fn publication_commits_real_album_and_done_once_after_persisted_stages() {
    let (mut app, original, writing, recordings, geometry) = publication_fixture().await;
    let mut sealer = Sealer::default();
    let done = store::publish(
        &mut app,
        &input(&original),
        &writing,
        &recordings,
        &geometry,
        &mut sealer,
    )
    .await
    .unwrap();
    assert_eq!(
        (done.status(), done.revision(), done.generation()),
        ("done", 6, 1)
    );
    assert_eq!(counts(&app).await, (1, 1, 1));
    assert!(store::load(&app, &input(&original), 1).await.unwrap() == done);
    assert_eq!(sealer.calls, 1);
    assert_eq!(
        store::publish(
            &mut app,
            &input(&original),
            &writing,
            &recordings,
            &geometry,
            &mut sealer,
        )
        .await
        .err(),
        Some(store::PublicationError::Checkpoint(StoreError::Stale))
    );
    assert_eq!(
        store::publish(
            &mut app,
            &input(&original),
            &done,
            &recordings,
            &geometry,
            &mut sealer,
        )
        .await
        .err(),
        Some(store::PublicationError::Checkpoint(StoreError::Invalid))
    );
    assert_eq!(sealer.calls, 1);
    assert_eq!(counts(&app).await, (1, 1, 1));
}

#[tokio::test]
async fn publication_refuses_stale_full_checkpoint_revision_generation_and_rejection_before_sealing(
) {
    let (mut app, original, writing, recordings, geometry) = publication_fixture().await;
    let mut sealer = Sealer::default();
    for mode in 0..4 {
        let mut cp = writing.checkpoint().clone();
        let current = match mode {
            0 => {
                // Same counters/status/identity, different complete checkpoint.
                let mut encoded: serde_json::Value =
                    serde_json::from_str(&writing.encode().unwrap()).unwrap();
                encoded["checkpoint"]["failure_code"] = "parse_failed".into();
                Snapshot::decode(
                    &encoded.to_string(),
                    writing.revision(),
                    &identity(&original),
                )
                .unwrap()
            }
            1 => writing.successor(cp, false, &identity(&original)).unwrap(),
            2 => writing.successor(cp, true, &identity(&original)).unwrap(),
            _ => {
                cp.state = JobState::Rejected;
                cp.admin_release_mbid = None;
                cp.probe_tally = None;
                writing.successor(cp, true, &identity(&original)).unwrap()
            }
        };
        // Simulated authority mutations are not the missing manual adapter.
        seed(&app, &current).await;
        assert_eq!(
            store::publish(
                &mut app,
                &input(&original),
                &writing,
                &recordings,
                &geometry,
                &mut sealer,
            )
            .await
            .err(),
            Some(store::PublicationError::Checkpoint(StoreError::Stale))
        );
        assert_eq!(sealer.calls, 0);
        assert_eq!(counts(&app).await, (0, 0, 0));
        assert!(
            store::load(&app, &input(&original), current.generation())
                .await
                .unwrap()
                == current
        );
    }
}

#[tokio::test]
async fn publication_rolls_back_album_if_done_update_errors_or_affects_no_row() {
    let (mut app, original, writing, recordings, geometry) = publication_fixture().await;
    let mut sealer = Sealer::default();
    for mode in 0..2 {
        let trigger = if mode == 0 {
            "CREATE FUNCTION pg_temp.refuse_done() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'refused'; END $$"
        } else {
            "CREATE OR REPLACE FUNCTION pg_temp.refuse_done() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$"
        };
        app.batch_execute(trigger).await.unwrap();
        app.batch_execute("CREATE TRIGGER refuse_done BEFORE UPDATE ON ingest_jobs FOR EACH ROW EXECUTE FUNCTION pg_temp.refuse_done()").await.unwrap();
        let error = store::publish(
            &mut app,
            &input(&original),
            &writing,
            &recordings,
            &geometry,
            &mut sealer,
        )
        .await
        .err();
        assert_eq!(
            error,
            Some(store::PublicationError::Checkpoint(if mode == 0 {
                StoreError::Database
            } else {
                StoreError::Stale
            }))
        );
        assert_eq!(counts(&app).await, (0, 0, 0));
        assert!(store::load(&app, &input(&original), 1).await.unwrap() == writing);
        app.batch_execute("DROP TRIGGER refuse_done ON ingest_jobs")
            .await
            .unwrap();
    }
    store::publish(
        &mut app,
        &input(&original),
        &writing,
        &recordings,
        &geometry,
        &mut sealer,
    )
    .await
    .unwrap();
    assert_eq!(counts(&app).await, (1, 1, 1));
}

#[tokio::test]
async fn publication_seal_error_and_cancellation_leave_no_album_or_done() {
    let (mut app, original, writing, recordings, geometry) = publication_fixture().await;
    let mut sealer = Sealer {
        fail: true,
        ..Sealer::default()
    };
    assert_eq!(
        store::publish(
            &mut app,
            &input(&original),
            &writing,
            &recordings,
            &geometry,
            &mut sealer,
        )
        .await
        .err(),
        Some(store::PublicationError::Album(WriteError::Seal))
    );
    assert_eq!(counts(&app).await, (0, 0, 0));
    assert!(store::load(&app, &input(&original), 1).await.unwrap() == writing);
    sealer.fail = false;
    sealer.pending = true;
    assert!(tokio::time::timeout(
        std::time::Duration::from_secs(1),
        store::publish(
            &mut app,
            &input(&original),
            &writing,
            &recordings,
            &geometry,
            &mut sealer,
        )
    )
    .await
    .is_err());
    assert_eq!(
        sealer.calls, 2,
        "cancellation must reach the sealer after track insertion"
    );
    assert_eq!(counts(&app).await, (0, 0, 0));
    assert!(store::load(&app, &input(&original), 1).await.unwrap() == writing);
    sealer.pending = false;
    store::publish(
        &mut app,
        &input(&original),
        &writing,
        &recordings,
        &geometry,
        &mut sealer,
    )
    .await
    .unwrap();
    assert_eq!(counts(&app).await, (1, 1, 1));
}

#[tokio::test]
async fn review_controls_rotate_generation_and_clear_previous_match_probe_and_failure_evidence() {
    for approve in [false, true] {
        let (mut app, original, writing, _, _) = publication_fixture().await;
        let mut cp = writing.checkpoint().clone();
        cp.state = JobState::NeedsReview;
        cp.attempt = nntune_ingest::jobs::MAX_ATTEMPTS;
        cp.failure_code = Some("probe_failed".into());
        cp.failure_reason = Some("retry budget exhausted".into());
        let parked = writing.successor(cp, false, &identity(&original)).unwrap();
        seed(&app, &parked).await;
        let action = if approve {
            Control::ApproveReview {
                release: OTHER_RELEASE,
            }
        } else {
            Control::RetryReview
        };
        let saved = store::apply_control(&mut app, &input(&original), &parked, action)
            .await
            .unwrap();
        assert_eq!(
            (saved.revision(), saved.generation(), saved.status()),
            (7, 7, "running")
        );
        let cp = saved.checkpoint();
        assert_eq!(cp.attempt, 1);
        assert!(cp.failure_code.is_none() && cp.failure_reason.is_none());
        assert_eq!(
            (&cp.job_id, &cp.idempotency_key),
            (
                &parked.checkpoint().job_id,
                &parked.checkpoint().idempotency_key
            )
        );
        if approve {
            assert_eq!(cp.state, JobState::Probing);
            assert_eq!(cp.admin_release_mbid.as_deref(), Some(OTHER_RELEASE));
            let tally = cp.probe_tally.as_ref().unwrap();
            assert_eq!(tally.total_batches, 1);
            assert!(tally.missing_by_batch.is_empty());
            assert!(!admin_gates::publish_ready(cp));
        } else {
            assert_eq!(cp.state, JobState::Matching);
            assert!(cp.admin_release_mbid.is_none() && cp.probe_tally.is_none());
        }
        assert_eq!(
            store::load(&app, &input(&original), 1).await.err(),
            Some(StoreError::Stale)
        );
        assert_eq!(
            store::apply_control(&mut app, &input(&original), &parked, action)
                .await
                .err(),
            Some(StoreError::Stale)
        );
        assert!(store::load(&app, &input(&original), 7).await.unwrap() == saved);
        assert_eq!(counts(&app).await, (0, 0, 0));
    }
}

#[tokio::test]
async fn controls_enforce_existing_states_and_never_restart_terminal_jobs() {
    let (mut app, original, writing, _, _) = publication_fixture().await;
    for state in [
        JobState::Queued,
        JobState::Parsing,
        JobState::Matching,
        JobState::Probing,
        JobState::Writing,
        JobState::NeedsReview,
        JobState::NeedsArchiveRead,
        JobState::Rejected,
        JobState::Failed,
        JobState::Done,
    ] {
        let mut cp = writing.checkpoint().clone();
        cp.state = state;
        if matches!(
            state,
            JobState::Queued | JobState::Parsing | JobState::Matching
        ) {
            cp.admin_release_mbid = None;
            cp.probe_tally = None;
        }
        let expected = writing.successor(cp, false, &identity(&original)).unwrap();
        for (action, allowed, next_state) in [
            (
                Control::ApproveReview { release: RELEASE },
                state == JobState::NeedsReview,
                JobState::Probing,
            ),
            (
                Control::RetryReview,
                state == JobState::NeedsReview,
                JobState::Matching,
            ),
            (
                Control::ResumeArchive,
                state == JobState::NeedsArchiveRead,
                JobState::Parsing,
            ),
            (
                Control::Reject,
                !nntune_ingest::jobs::is_terminal(state),
                JobState::Rejected,
            ),
        ] {
            seed(&app, &expected).await;
            let result = store::apply_control(&mut app, &input(&original), &expected, action).await;
            if allowed {
                let saved = result.unwrap();
                assert_eq!(saved.checkpoint().state, next_state);
                assert_eq!((saved.revision(), saved.generation()), (7, 7));
                assert!(store::load(&app, &input(&original), 7).await.unwrap() == saved);
            } else {
                assert_eq!(result.err(), Some(StoreError::Invalid));
                assert!(store::load(&app, &input(&original), 1).await.unwrap() == expected);
            }
        }
    }
}

#[tokio::test]
async fn rejection_invalidates_old_publication_and_done_refuses_later_controls() {
    let (mut app, original, writing, recordings, geometry) = publication_fixture().await;
    let rejected = store::apply_control(&mut app, &input(&original), &writing, Control::Reject)
        .await
        .unwrap();
    assert_eq!(
        (
            rejected.status(),
            rejected.revision(),
            rejected.generation()
        ),
        ("rejected", 6, 6)
    );
    assert!(rejected.checkpoint().admin_release_mbid.is_none());
    assert!(rejected.checkpoint().probe_tally.is_none());
    let mut sealer = Sealer::default();
    assert_eq!(
        store::publish(
            &mut app,
            &input(&original),
            &writing,
            &recordings,
            &geometry,
            &mut sealer
        )
        .await
        .err(),
        Some(store::PublicationError::Checkpoint(StoreError::Stale))
    );
    assert_eq!(sealer.calls, 0);
    assert_eq!(counts(&app).await, (0, 0, 0));
    // A separate fixture proves the opposite settled order, not a concurrent race.
    let (mut app, original, writing, recordings, geometry) = publication_fixture().await;
    let done = store::publish(
        &mut app,
        &input(&original),
        &writing,
        &recordings,
        &geometry,
        &mut sealer,
    )
    .await
    .unwrap();
    assert_eq!(
        store::apply_control(&mut app, &input(&original), &writing, Control::Reject)
            .await
            .err(),
        Some(StoreError::Stale)
    );
    assert_eq!(
        store::apply_control(&mut app, &input(&original), &done, Control::Reject)
            .await
            .err(),
        Some(StoreError::Invalid)
    );
    assert!(store::load(&app, &input(&original), 1).await.unwrap() == done);
    assert_eq!(counts(&app).await, (1, 1, 1));
}

#[tokio::test]
async fn control_cas_compares_full_snapshot_revision_generation_and_original_linkage() {
    let (mut app, original, writing, _, _) = publication_fixture().await;
    for mode in 0..3 {
        let current = match mode {
            0 => {
                let mut cp = writing.checkpoint().clone();
                cp.failure_code = Some("write_failed".into());
                let mut encoded: serde_json::Value =
                    serde_json::from_str(&writing.encode().unwrap()).unwrap();
                encoded["checkpoint"] = serde_json::to_value(cp).unwrap();
                Snapshot::decode(
                    &encoded.to_string(),
                    writing.revision(),
                    &identity(&original),
                )
                .unwrap()
            }
            1 => writing
                .successor(writing.checkpoint().clone(), false, &identity(&original))
                .unwrap(),
            _ => writing
                .successor(writing.checkpoint().clone(), true, &identity(&original))
                .unwrap(),
        };
        seed(&app, &current).await;
        assert_eq!(
            store::apply_control(&mut app, &input(&original), &writing, Control::Reject)
                .await
                .err(),
            Some(StoreError::Stale)
        );
        assert!(
            store::load(&app, &input(&original), current.generation())
                .await
                .unwrap()
                == current
        );
    }
    seed(&app, &writing).await;
    app.batch_execute("UPDATE ingest_originals SET nzb_bytes = nzb_bytes + 1")
        .await
        .unwrap();
    assert_eq!(
        store::apply_control(&mut app, &input(&original), &writing, Control::Reject)
            .await
            .err(),
        Some(StoreError::Stale)
    );
    app.batch_execute("UPDATE ingest_originals SET nzb_bytes = nzb_bytes - 1; UPDATE ingest_jobs SET source = 'upload'").await.unwrap();
    assert_eq!(
        store::apply_control(&mut app, &input(&original), &writing, Control::Reject)
            .await
            .err(),
        Some(StoreError::Stale)
    );
}

#[tokio::test]
async fn control_sql_failure_zero_row_and_overflow_preserve_status_checkpoint_and_generation() {
    let (mut app, original, writing, _, _) = publication_fixture().await;
    for mode in 0..2 {
        let trigger = if mode == 0 {
            "CREATE FUNCTION pg_temp.refuse_control() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'refused'; END $$"
        } else {
            "CREATE OR REPLACE FUNCTION pg_temp.refuse_control() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$"
        };
        app.batch_execute(trigger).await.unwrap();
        app.batch_execute("CREATE TRIGGER refuse_control BEFORE UPDATE ON ingest_jobs FOR EACH ROW EXECUTE FUNCTION pg_temp.refuse_control()").await.unwrap();
        assert_eq!(
            store::apply_control(&mut app, &input(&original), &writing, Control::Reject)
                .await
                .err(),
            Some(if mode == 0 {
                StoreError::Database
            } else {
                StoreError::Stale
            })
        );
        assert!(store::load(&app, &input(&original), 1).await.unwrap() == writing);
        app.batch_execute("DROP TRIGGER refuse_control ON ingest_jobs")
            .await
            .unwrap();
    }
    app.execute(
        "UPDATE ingest_jobs SET admin_checkpoint_revision = $1",
        &[&i64::MAX],
    )
    .await
    .unwrap();
    let limit = store::load(&app, &input(&original), 1).await.unwrap();
    assert_eq!(
        store::apply_control(&mut app, &input(&original), &limit, Control::Reject)
            .await
            .err(),
        Some(StoreError::Invalid)
    );
    assert!(store::load(&app, &input(&original), 1).await.unwrap() == limit);
    assert_eq!(counts(&app).await, (0, 0, 0));
}

#[tokio::test]
async fn store_database_failure_rolls_back_checkpoint_and_status_together() {
    let (mut app, original, initial) = seeded().await;
    app.batch_execute("CREATE FUNCTION pg_temp.refuse_checkpoint() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'refused'; END $$; CREATE TRIGGER refuse_checkpoint BEFORE UPDATE ON ingest_jobs FOR EACH ROW EXECUTE FUNCTION pg_temp.refuse_checkpoint()").await.unwrap();
    let mut next = initial.checkpoint().clone();
    next.state = JobState::Parsing;
    assert_eq!(
        store::persist_stage(&mut app, &input(&original), &initial, &next)
            .await
            .err(),
        Some(StoreError::Database)
    );
    assert!(store::load(&app, &input(&original), 1).await.unwrap() == initial);
    app.batch_execute("DROP TRIGGER refuse_checkpoint ON ingest_jobs")
        .await
        .unwrap();
    let saved = store::persist_stage(&mut app, &input(&original), &initial, &next)
        .await
        .unwrap();
    assert_eq!(
        (saved.status(), saved.revision(), saved.generation()),
        ("running", 2, 1)
    );
    assert!(store::load(&app, &input(&original), 1).await.unwrap() == saved);
}
