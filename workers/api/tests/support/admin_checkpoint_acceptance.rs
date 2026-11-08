//! Disposable PostgreSQL stage-CAS tests. Test seeding is not an admission or
//! manual-control adapter, and temporary columns do not prove migration checks.
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
