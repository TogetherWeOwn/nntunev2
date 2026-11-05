//! Real validation adapter and proposed migration on CI's disposable PostgreSQL.
//! Explicit feature/config required; never a live API request or staging apply.

use nntune_mb::videolinks::validation::{
    refresh_due, BatchValidation, PgValidationStore, ValidationClient, ValidationError,
    ValidationResponse, ValidationStore, MAX_VALIDATIONS_PER_RUN,
};
use nntune_mb::videolinks::VALIDATION_TTL_S;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::time::Duration;
use tokio_postgres::{Client, Config, NoTls};

const NOW: i64 = 1_700_000_000;
const MIGRATION: &str = include_str!("../../../migrations/0035_videolinks_curation.sql");

fn disposable_config(url: &str) -> Result<Config, &'static str> {
    let mut config: Config = url.parse().map_err(|_| "invalid test database URL")?;
    if config.get_dbname() != Some("nntune_videolinks_ci") {
        return Err("dedicated disposable database required");
    }
    if config.get_hosts().is_empty()
        || !config.get_hosts().iter().all(|host| match host {
            tokio_postgres::config::Host::Tcp(host) => {
                host == "localhost"
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            }
            _ => false,
        })
        || !config.get_hostaddrs().iter().all(|addr| addr.is_loopback())
    {
        return Err("test database hosts and addresses must be local");
    }
    config.connect_timeout(Duration::from_secs(3));
    Ok(config)
}

#[test]
fn database_config_refuses_non_disposable_targets() {
    let base = "postgresql://postgres@localhost/nntune_videolinks_ci";
    for url in [
        base.to_owned(),
        format!("{base}?hostaddr=127.0.0.1"),
        "postgresql://postgres@[::1]/nntune_videolinks_ci".to_owned(),
    ] {
        assert!(disposable_config(&url).is_ok());
    }
    for url in [
        format!("{base}?dbname=nntune_ci"),
        format!("{base}?hostaddr=203.0.113.10"),
        format!("{base}?hostaddr=127.0.0.1,203.0.113.10"),
        format!("{base}?host=remote.example.test"),
        format!("{base}?host=%2Fvar%2Frun%2Fpostgresql"),
        "postgresql://postgres@remote.example.test/nntune_videolinks_ci".to_owned(),
    ] {
        assert!(disposable_config(&url).is_err());
    }
}

async fn connect(config: &Config) -> Client {
    let (db, connection) = config
        .connect(NoTls)
        .await
        .expect("scratch database connect");
    tokio::spawn(async move { connection.await.expect("scratch database driver") });
    db.batch_execute("SET statement_timeout = '5s'")
        .await
        .unwrap();
    db
}

struct FixtureClient {
    replies: RefCell<VecDeque<ValidationResponse>>,
    calls: RefCell<Vec<Vec<String>>>,
}

impl FixtureClient {
    fn new(replies: Vec<(u16, &str)>) -> Self {
        Self {
            replies: RefCell::new(
                replies
                    .into_iter()
                    .map(|(status, body)| ValidationResponse {
                        status,
                        body: body.into(),
                    })
                    .collect(),
            ),
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl ValidationClient for FixtureClient {
    async fn videos_list(&self, ids: &[String]) -> Result<ValidationResponse, ValidationError> {
        self.calls.borrow_mut().push(ids.to_vec());
        Ok(self
            .replies
            .borrow_mut()
            .pop_front()
            .expect("bounded request"))
    }
}

#[derive(Default)]
struct CaptureBatch(Option<BatchValidation>);

impl ValidationStore for CaptureBatch {
    async fn purge_expired(&mut self, _: i64) -> Result<u64, ValidationError> {
        Ok(0)
    }

    async fn due_video_ids(&mut self, _: i64, _: usize) -> Result<Vec<String>, ValidationError> {
        Ok(vec!["v0000000000".into()])
    }

    async fn apply_batch(&mut self, batch: &BatchValidation) -> Result<u64, ValidationError> {
        self.0 = Some(batch.clone());
        Ok(1)
    }
}

async fn checked_batch(now: i64) -> BatchValidation {
    let mut capture = CaptureBatch::default();
    let client = FixtureClient::new(vec![(200, r#"{"items":[]}"#)]);
    refresh_due(&mut capture, &client, now).await.unwrap();
    capture.0.unwrap()
}

async fn seed(db: &Client, count: i32) {
    db.execute(
        "INSERT INTO public.video_link_candidates (recording_mbid, video_id, source)
         SELECT '00000000-0000-0000-0000-000000000001',
                'v' || lpad(i::text, 10, '0'), 'mb_url'
         FROM generate_series(0, $1::integer - 1) AS i",
        &[&count],
    )
    .await
    .unwrap();
}

async fn states(db: &Client) -> Vec<(String, String, Option<String>, i64, i64)> {
    db.query(
        "SELECT video_id, status, title, validated_at, expires_at
         FROM public.video_link_candidates ORDER BY video_id, recording_mbid",
        &[],
    )
    .await
    .unwrap()
    .into_iter()
    .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4)))
    .collect()
}

async fn reset_candidates(db: &Client) {
    // This invocation created the table in the dedicated empty CI database.
    db.batch_execute("DELETE FROM public.video_link_candidates")
        .await
        .unwrap();
}

async fn writes_and_replays(db: &mut Client) {
    seed(db, 5).await;
    db.batch_execute(
        "INSERT INTO public.video_link_candidates (recording_mbid, video_id, source)
         VALUES ('00000000-0000-0000-0000-000000000002', 'v0000000000', 'wikidata');
         INSERT INTO public.video_user_submissions (recording_mbid, video_id, submitted_by)
         VALUES ('00000000-0000-0000-0000-000000000001', 'v0000000005', 'synthetic-user')",
    )
    .await
    .unwrap();
    let client = FixtureClient::new(vec![(
        200,
        r#"{"items":[
        {"id":"v0000000000","status":{"embeddable":true,"privacyStatus":"public"},
         "snippet":{"title":"fixture title","channelId":"fixture channel"},
         "contentDetails":{"duration":"PT3M"}},
        {"id":"v0000000001","status":{"embeddable":true,"privacyStatus":"private"}},
        {"id":"v0000000002","status":{"embeddable":false,"privacyStatus":"public"}},
        {"id":"v0000000004","status":{"embeddable":true,"privacyStatus":"unlisted"}}
    ]}"#,
    )]);
    let report = refresh_due(&mut PgValidationStore::new(db), &client, NOW)
        .await
        .unwrap();
    assert_eq!(
        (report.requested_ids, report.batches, report.updated_rows),
        (5, 1, 6)
    );
    assert_eq!(
        client.calls.borrow()[0].len(),
        5,
        "one ID shared by recordings"
    );
    let rows = states(db).await;
    assert_eq!(
        rows.iter().map(|r| r.1.as_str()).collect::<Vec<_>>(),
        [
            "validated",
            "validated",
            "rejected",
            "rejected",
            "rejected",
            "validated"
        ]
    );
    assert!(rows
        .iter()
        .all(|r| r.3 == NOW && r.4 == NOW + VALIDATION_TTL_S));
    assert_eq!(rows[0].2.as_deref(), Some("fixture title"));
    let metadata = db
        .query_one(
            "SELECT channel_id, duration_secs, embeddable FROM public.video_link_candidates
         WHERE video_id='v0000000000' LIMIT 1",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(metadata.get::<_, String>(0), "fixture channel");
    assert_eq!(metadata.get::<_, i32>(1), 180);
    assert!(metadata.get::<_, bool>(2));
    let missing = db.query_one(
        "SELECT title IS NULL AND channel_id IS NULL AND duration_secs IS NULL
                AND embeddable IS NULL FROM public.video_link_candidates WHERE video_id='v0000000003'",
        &[],
    ).await.unwrap();
    assert!(missing.get::<_, bool>(0));
    db.batch_execute(MIGRATION).await.unwrap();
    assert_eq!(
        states(db).await,
        rows,
        "replay preserves recorded validation"
    );
    let client = FixtureClient::new(Vec::new());
    let report = refresh_due(&mut PgValidationStore::new(db), &client, NOW + 1)
        .await
        .unwrap();
    assert_eq!(
        report.requested_ids, 0,
        "fresh rejected rows are not hammered"
    );
    assert!(client.calls.borrow().is_empty());
}

async fn expiry_and_due_bounds(db: &mut Client) {
    reset_candidates(db).await;
    seed(db, 5).await;
    db.execute(
        "UPDATE public.video_link_candidates
         SET status=CASE WHEN video_id='v0000000001' THEN 'rejected' ELSE 'validated' END,
             title='old title', channel_id='old channel', duration_secs=180, embeddable=true,
             validated_at=$1, expires_at=$2::bigint + CASE video_id
                WHEN 'v0000000000' THEN -1 WHEN 'v0000000001' THEN 0
                WHEN 'v0000000002' THEN 1 WHEN 'v0000000003' THEN 86400 ELSE 86401 END",
        &[&(NOW - VALIDATION_TTL_S), &NOW],
    )
    .await
    .unwrap();
    let mut store = PgValidationStore::new(db);
    assert_eq!(store.purge_expired(NOW).await.unwrap(), 2);
    assert_eq!(
        store.purge_expired(NOW).await.unwrap(),
        0,
        "idempotent purge"
    );
    assert_eq!(
        store.due_video_ids(NOW, usize::MAX).await.unwrap(),
        ["v0000000000", "v0000000001", "v0000000002", "v0000000003"]
    );
    let rows = db.query(
        "SELECT status, validated_at, expires_at,
                title IS NULL AND channel_id IS NULL AND duration_secs IS NULL AND embeddable IS NULL
         FROM public.video_link_candidates ORDER BY video_id", &[]).await.unwrap();
    for row in &rows[..2] {
        assert_eq!(row.get::<_, String>(0), "expired");
        assert_eq!(row.get::<_, i64>(1), NOW - VALIDATION_TTL_S);
        assert_eq!(row.get::<_, i64>(2), 0);
        assert!(row.get::<_, bool>(3));
    }
    assert!(rows[2..].iter().all(|row| !row.get::<_, bool>(3)));

    // Cleanup commits even if the network is unavailable immediately after it.
    let client = FixtureClient::new(vec![(503, "not persisted")]);
    assert_eq!(
        refresh_due(&mut PgValidationStore::new(db), &client, NOW + 1).await,
        Err(ValidationError::HttpStatus(503))
    );
    assert_eq!(states(db).await[2].1, "expired");

    reset_candidates(db).await;
    seed(db, 205).await;
    let ids = PgValidationStore::new(db)
        .due_video_ids(NOW, usize::MAX)
        .await
        .unwrap();
    assert_eq!(ids.len(), MAX_VALIDATIONS_PER_RUN);
    assert_eq!(ids.first().unwrap(), "v0000000000");
    assert_eq!(ids.last().unwrap(), "v0000000199");
    assert!(PgValidationStore::new(db)
        .due_video_ids(NOW, 0)
        .await
        .unwrap()
        .is_empty());
}

async fn stale_writes_and_atomic_failure(db: &mut Client) {
    reset_candidates(db).await;
    seed(db, 2).await;
    let newer = checked_batch(NOW + 1).await;
    assert_eq!(
        PgValidationStore::new(db)
            .apply_batch(&newer)
            .await
            .unwrap(),
        1
    );
    let rows = states(db).await;
    for batch in [checked_batch(NOW).await, checked_batch(NOW + 1).await] {
        assert_eq!(
            PgValidationStore::new(db)
                .apply_batch(&batch)
                .await
                .unwrap(),
            0
        );
        assert_eq!(states(db).await, rows, "stale/same-second write is a no-op");
    }
    db.batch_execute(
        "ALTER TABLE public.video_link_candidates ADD CONSTRAINT fixture_reject_title
         CHECK (title IS DISTINCT FROM 'reject-in-fixture')",
    )
    .await
    .unwrap();
    let client = FixtureClient::new(vec![(
        200,
        r#"{"items":[
        {"id":"v0000000000","status":{"embeddable":true,"privacyStatus":"public"},
         "snippet":{"title":"would-change"}},
        {"id":"v0000000001","status":{"embeddable":true,"privacyStatus":"public"},
         "snippet":{"title":"reject-in-fixture"}}
    ]}"#,
    )]);
    // Both IDs must be due; cleanup may commit, but checked writes must not.
    db.execute(
        "UPDATE public.video_link_candidates SET expires_at=$1",
        &[&(NOW + 2)],
    )
    .await
    .unwrap();
    let before = states(db).await;
    assert_eq!(
        refresh_due(&mut PgValidationStore::new(db), &client, NOW + 2).await,
        Err(ValidationError::Store)
    );
    // The first row's cleanup is expected; neither checked update commits.
    let after = states(db).await;
    assert_eq!(after[0].1, "expired");
    assert_eq!(after[0].3, before[0].3);
    assert_eq!(after[1].1, "pending");
    assert_eq!(after[1].3, 0);
    assert!(after.iter().all(|row| row.2.is_none()));
    db.batch_execute(
        "ALTER TABLE public.video_link_candidates DROP CONSTRAINT fixture_reject_title",
    )
    .await
    .unwrap();
    // The rolled-back transaction did not poison the connection.
    assert_eq!(
        PgValidationStore::new(db)
            .apply_batch(&checked_batch(NOW + 3).await)
            .await
            .unwrap(),
        1
    );
}

async fn contention_is_bounded(db: &mut Client, config: &Config) {
    reset_candidates(db).await;
    seed(db, 1).await;
    let mut locker = connect(config).await;
    let tx = locker.transaction().await.unwrap();
    tx.query_one(
        "SELECT video_id FROM public.video_link_candidates FOR UPDATE",
        &[],
    )
    .await
    .unwrap();
    let before = states(db).await;
    let batch = checked_batch(NOW).await;
    let result = tokio::time::timeout(
        Duration::from_secs(4),
        PgValidationStore::new(db).apply_batch(&batch),
    )
    .await
    .expect("server statement timeout");
    assert_eq!(result, Err(ValidationError::Store));
    assert_eq!(states(db).await, before, "timed-out write did not commit");
    tx.commit().await.unwrap();
    assert_eq!(
        PgValidationStore::new(db)
            .apply_batch(&batch)
            .await
            .unwrap(),
        1
    );

    // A blocked expiry purge must stop the runner before its first API call.
    let tx = locker.transaction().await.unwrap();
    tx.query_one(
        "SELECT video_id FROM public.video_link_candidates FOR UPDATE",
        &[],
    )
    .await
    .unwrap();
    let client = FixtureClient::new(Vec::new());
    let result = tokio::time::timeout(
        Duration::from_secs(4),
        refresh_due(
            &mut PgValidationStore::new(db),
            &client,
            NOW + VALIDATION_TTL_S,
        ),
    )
    .await
    .expect("purge statement timeout");
    assert_eq!(result, Err(ValidationError::Store));
    assert!(client.calls.borrow().is_empty());
    tx.commit().await.unwrap();
}

async fn earlier_batch_survives(db: &mut Client) {
    reset_candidates(db).await;
    seed(db, 51).await;
    let client = FixtureClient::new(vec![(200, r#"{"items":[]}"#), (503, "not persisted")]);
    assert_eq!(
        refresh_due(&mut PgValidationStore::new(db), &client, NOW).await,
        Err(ValidationError::HttpStatus(503))
    );
    let rows = states(db).await;
    assert!(rows[..50]
        .iter()
        .all(|row| row.1 == "rejected" && row.3 == NOW));
    assert_eq!((rows[50].1.as_str(), rows[50].3), ("pending", 0));
    assert_eq!(
        client
            .calls
            .borrow()
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>(),
        [50, 1]
    );
}

#[test]
fn postgres_validation_store_contract() {
    let url = std::env::var("NNTUNE_VIDEOLINKS_TEST_DATABASE_URL")
        .expect("NNTUNE_VIDEOLINKS_TEST_DATABASE_URL must name CI's dedicated disposable database");
    let config = disposable_config(&url).expect("scratch database configuration");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(30), async {
                let mut db = connect(&config).await;
                let absent: bool = db
                    .query_one(
                        "SELECT to_regclass('public.video_link_candidates') IS NULL
                         AND to_regclass('public.video_user_submissions') IS NULL
                         AND to_regclass('public.video_coverage_snapshots') IS NULL",
                        &[],
                    )
                    .await
                    .unwrap()
                    .get(0);
                assert!(
                    absent,
                    "refuse pre-existing tables; CI must supply an empty disposable database"
                );
                db.batch_execute(MIGRATION).await.unwrap();
                writes_and_replays(&mut db).await;
                expiry_and_due_bounds(&mut db).await;
                stale_writes_and_atomic_failure(&mut db).await;
                contention_is_bounded(&mut db, &config).await;
                earlier_batch_survives(&mut db).await;
            })
            .await
            .expect("bounded PostgreSQL acceptance");
        });
}
