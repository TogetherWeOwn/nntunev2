//! Real overlapping transactions, not sequential stale-action simulation. The
//! shared schema copies only this test's isolated fixture; no migration/app DB
//! is touched. These cases do not establish ordered-schema or broker acceptance.
use super::super::super::database_client;
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio_postgres::Client;

static SCHEMA: AtomicUsize = AtomicUsize::new(0);

struct SharedRows {
    schema: String,
    observer: Client,
}

impl SharedRows {
    async fn copy_from(app: &Client) -> Self {
        let schema = format!(
            "admin_race_{}_{}",
            std::process::id(),
            SCHEMA.fetch_add(1, Ordering::Relaxed)
        );
        // Identifiers above come exclusively from process/counter integers.
        app.batch_execute(&format!("CREATE SCHEMA {schema}"))
            .await
            .unwrap();
        for table in [
            "ingest_jobs",
            "ingest_originals",
            "tracks",
            "track_sources",
            "library",
        ] {
            app.batch_execute(&format!(
                "CREATE TABLE {schema}.{table} (LIKE pg_temp.{table} INCLUDING ALL);
                 INSERT INTO {schema}.{table} SELECT * FROM pg_temp.{table}"
            ))
            .await
            .unwrap();
        }
        app.batch_execute(&format!(
            "ALTER TABLE {schema}.ingest_originals ADD FOREIGN KEY (job_id) REFERENCES {schema}.ingest_jobs(id);
             ALTER TABLE {schema}.track_sources ADD FOREIGN KEY (track_id) REFERENCES {schema}.tracks(id),
                 ADD FOREIGN KEY (source_job_id) REFERENCES {schema}.ingest_jobs(id)"
        )).await.unwrap();
        let rows = Self {
            schema,
            observer: database_client().await,
        };
        rows.select_schema(&rows.observer).await;
        rows
    }

    async fn select_schema(&self, client: &Client) {
        client
            .batch_execute(&format!("SET search_path TO {}, pg_catalog", self.schema))
            .await
            .unwrap();
    }

    async fn client(&self) -> Client {
        let client = database_client().await;
        self.select_schema(&client).await;
        client
    }

    async fn cleanup(self) {
        self.observer
            .batch_execute(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .await
            .unwrap();
        let exists: bool = self
            .observer
            .query_one(
                "SELECT EXISTS (SELECT FROM pg_namespace WHERE nspname = $1)",
                &[&self.schema],
            )
            .await
            .unwrap()
            .get(0);
        assert!(!exists, "scratch schema must be removed");
    }
}

async fn pid(client: &Client) -> i32 {
    client
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0)
}

// Observe the exact blocker, not elapsed wall time or merely an unfinished
// future. The timeout bounds a failed test; it is never evidence of contention.
async fn blocked_by(observer: &Client, waiting: i32, holding: i32) -> bool {
    tokio::time::timeout(Duration::from_secs(1), async {
        let mut tick = tokio::time::interval(Duration::from_millis(10));
        loop {
            tick.tick().await;
            let blocked: bool = observer
                .query_one(
                    "SELECT $2 = ANY(pg_blocking_pids($1))",
                    &[&waiting, &holding],
                )
                .await
                .unwrap()
                .get(0);
            if blocked {
                return;
            }
        }
    })
    .await
    .is_ok()
}

struct PausedSealer {
    entered: Option<oneshot::Sender<()>>,
    release: oneshot::Receiver<()>,
    inner: Sealer,
}

impl SourceSealer for PausedSealer {
    async fn seal(&mut self, source: &OriginalSource) -> Result<SourceEnvelope, WriteError> {
        self.entered.take().unwrap().send(()).unwrap();
        (&mut self.release).await.unwrap();
        self.inner.seal(source).await
    }
}

#[tokio::test]
async fn publication_holds_job_lock_and_concurrent_rejection_loses_after_done_commit() {
    let (app, original, writing, recordings, geometry) = publication_fixture().await;
    let rows = SharedRows::copy_from(&app).await;
    let mut publisher = rows.client().await;
    let mut controller = rows.client().await;
    let publisher_pid = pid(&publisher).await;
    let controller_pid = pid(&controller).await;
    let (entered, started) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let mut sealer = PausedSealer {
        entered: Some(entered),
        release: released,
        inner: Sealer::default(),
    };
    let original_input = input(&original);
    let publication = store::publish(
        &mut publisher,
        &original_input,
        &writing,
        &recordings,
        &geometry,
        &mut sealer,
    );
    let rejection = async {
        started.await.unwrap();
        let original = input(&original);
        let action = store::apply_control(&mut controller, &original, &writing, Control::Reject);
        tokio::pin!(action);
        let (early, blocked) = tokio::select! {
            result = &mut action => (Some(result), false),
            blocked = blocked_by(&rows.observer, controller_pid, publisher_pid) => (None, blocked),
        };
        release.send(()).unwrap();
        let result = match early {
            Some(result) => result,
            None => action.await,
        };
        (result, blocked)
    };
    let settled = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(publication, rejection)
    })
    .await;
    let saved = store::load(&rows.observer, &input(&original), writing.generation())
        .await
        .unwrap();
    let album_counts = counts(&rows.observer).await;
    rows.cleanup().await;

    let (published, (rejected, blocked)) = settled.expect("settlement must finish after release");
    assert!(
        blocked,
        "rejection must actually wait on publication's job lock"
    );
    let done = published.unwrap();
    assert!(matches!(rejected, Err(StoreError::Stale)));
    assert!(saved == done);
    assert_eq!(done.checkpoint().state, JobState::Done);
    assert_eq!(done.revision(), writing.revision() + 1);
    assert_eq!(done.generation(), writing.generation());
    assert_eq!(album_counts, (1, 1, 1));
    assert_eq!(sealer.inner.calls, 1);
    // The old temporary fixture is not the shared authority used by the race.
    assert_eq!(counts(&app).await, (0, 0, 0));
}

#[tokio::test]
async fn rejection_holds_job_lock_and_concurrent_publication_loses_without_sealing() {
    let (app, original, writing, recordings, geometry) = publication_fixture().await;
    let rows = SharedRows::copy_from(&app).await;
    let mut publisher = rows.client().await;
    let mut controller = rows.client().await;
    let publisher_pid = pid(&publisher).await;
    let controller_pid = pid(&controller).await;
    let observer_pid = pid(&rows.observer).await;
    let gate = i64::from(observer_pid);
    rows.observer
        .query_one("SELECT pg_advisory_lock($1)", &[&gate])
        .await
        .unwrap();
    // Pause the real control transaction after its row lock, before its update
    // commits. Only this scratch schema has the trigger. No production seam.
    rows.observer
        .batch_execute(&format!(
            "CREATE FUNCTION {}.hold_control() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN PERFORM pg_advisory_xact_lock({gate}::bigint); RETURN NEW; END $$;
         CREATE TRIGGER hold_control BEFORE UPDATE ON ingest_jobs FOR EACH ROW
             WHEN (NEW.status = 'rejected') EXECUTE FUNCTION {}.hold_control()",
            rows.schema, rows.schema,
        ))
        .await
        .unwrap();
    let original_input = input(&original);
    let rejection =
        store::apply_control(&mut controller, &original_input, &writing, Control::Reject);
    let mut sealer = Sealer::default();
    let publication = async {
        let control_blocked = blocked_by(&rows.observer, controller_pid, observer_pid).await;
        let original = input(&original);
        let action = store::publish(
            &mut publisher,
            &original,
            &writing,
            &recordings,
            &geometry,
            &mut sealer,
        );
        tokio::pin!(action);
        let (early, publish_blocked) = tokio::select! {
            result = &mut action => (Some(result), false),
            blocked = blocked_by(&rows.observer, publisher_pid, controller_pid) => (None, blocked),
        };
        let unlocked: bool = rows
            .observer
            .query_one("SELECT pg_advisory_unlock($1)", &[&gate])
            .await
            .unwrap()
            .get(0);
        let result = match early {
            Some(result) => result,
            None => action.await,
        };
        (result, control_blocked, publish_blocked, unlocked)
    };
    let settled = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(rejection, publication)
    })
    .await;
    if settled.is_err() {
        rows.observer
            .query_one("SELECT pg_advisory_unlock($1)", &[&gate])
            .await
            .unwrap();
    }
    let reloaded = store::load(&rows.observer, &input(&original), writing.generation()).await;
    let authority = authority_row(&rows.observer).await;
    let album_counts = counts(&rows.observer).await;
    rows.cleanup().await;

    let (rejected, (published, control_blocked, publish_blocked, unlocked)) =
        settled.expect("settlement must finish after release");
    assert!(
        control_blocked,
        "control must hold its job lock before publication starts"
    );
    assert!(
        publish_blocked,
        "publication must actually wait on control's job lock"
    );
    assert!(unlocked);
    let rejected = rejected.unwrap();
    assert!(matches!(
        published,
        Err(store::PublicationError::Checkpoint(StoreError::Stale))
    ));
    assert!(matches!(reloaded, Err(StoreError::Stale)));
    assert_eq!(rejected.checkpoint().state, JobState::Rejected);
    assert_eq!(rejected.revision(), writing.revision() + 1);
    assert_eq!(rejected.generation(), writing.generation() + 1);
    assert_eq!(authority.0, "rejected");
    assert_eq!(authority.1, rejected.revision());
    let decoded =
        Snapshot::decode(&authority.2.unwrap(), authority.1, &identity(&original)).unwrap();
    assert!(decoded == rejected);
    assert_eq!(album_counts, (0, 0, 0));
    assert_eq!(sealer.calls, 0);
}
