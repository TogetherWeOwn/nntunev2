//! Real validation adapter and proposed migration on CI's disposable PostgreSQL.
//! Explicit feature/config required; never a live API request or staging apply.

use nntune_mb::videolinks::harvest::{
    admit_mirror_pages, HarvestError, MirrorCursor, MirrorHarvestError, PgMirrorSource,
    MAX_MIRROR_PAGES_PER_RUN, MAX_MIRROR_ROWS_PER_PAGE,
};
use nntune_mb::videolinks::moderation::{
    PgSubmissionStore, SubmissionStoreError, MAX_PENDING_SUBMISSIONS,
};
use nntune_mb::videolinks::storage::{
    CandidateStoreError, PgCandidateStore, MAX_LINKS_PER_RECORDING, MAX_RECORDINGS_PER_READ,
};
use nntune_mb::videolinks::validation::{
    refresh_due, BatchValidation, PgValidationStore, ValidationClient, ValidationError,
    ValidationResponse, ValidationStore, MAX_VALIDATIONS_PER_RUN,
};
use nntune_mb::videolinks::{Candidate, SubmissionStatus, VideoSource, VALIDATION_TTL_S};
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

fn external_candidate(id: usize, source: VideoSource) -> Candidate {
    Candidate {
        recording_mbid: "abcdef01-2345-6789-abcd-0123456789ab".into(),
        video_id: format!("v{id:010}"),
        source,
    }
}

async fn candidate_admission_contract(db: &mut Client) {
    reset_candidates(db).await;
    let mut upper = external_candidate(0, VideoSource::MbUrl);
    upper.recording_mbid.make_ascii_uppercase();
    assert_eq!(
        PgCandidateStore::new(db)
            .admit_external_candidates(&[
                external_candidate(0, VideoSource::ListenBrainz),
                upper,
                external_candidate(1, VideoSource::Wikidata),
                external_candidate(2, VideoSource::MbUrl),
                external_candidate(3, VideoSource::ListenBrainz),
            ])
            .await
            .unwrap(),
        4,
        "canonical duplicate keys are admitted once"
    );
    let source: String = db
        .query_one(
            "SELECT source FROM public.video_link_candidates WHERE video_id='v0000000003'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        source, "listenbrainz",
        "database label differs from the enum's snake-case form"
    );
    let client = FixtureClient::new(vec![(
        200,
        r#"{"items":[
        {"id":"v0000000000","status":{"embeddable":true,"privacyStatus":"public"},"snippet":{"title":"preserved"}},
        {"id":"v0000000001","status":{"embeddable":true,"privacyStatus":"public"},"snippet":{"title":"also preserved"}}
    ]}"#,
    )]);
    refresh_due(&mut PgValidationStore::new(db), &client, NOW)
        .await
        .unwrap();
    let before = states(db).await;
    let preserved: String = db.query_one(
        "SELECT ROW(title, channel_id, duration_secs, embeddable, validated_at, expires_at, first_seen_at)::text
         FROM public.video_link_candidates WHERE video_id='v0000000001'", &[]).await.unwrap().get(0);
    assert_eq!(
        PgCandidateStore::new(db)
            .admit_external_candidates(&[
                external_candidate(0, VideoSource::ListenBrainz),
                external_candidate(1, VideoSource::MbUrl),
            ])
            .await
            .unwrap(),
        1,
        "only a provenance upgrade writes"
    );
    assert_eq!(
        states(db).await,
        before,
        "admission does not reset checked data"
    );
    let row = db.query_one(
        "SELECT source, ROW(title, channel_id, duration_secs, embeddable, validated_at, expires_at, first_seen_at)::text
         FROM public.video_link_candidates WHERE video_id='v0000000001'", &[]).await.unwrap();
    assert_eq!(row.get::<_, String>(0), "mb_url");
    assert_eq!(row.get::<_, String>(1), preserved);
    assert_eq!(
        PgCandidateStore::new(db)
            .admit_external_candidates(&[external_candidate(1, VideoSource::MbUrl),])
            .await
            .unwrap(),
        0,
        "replay is a no-op"
    );

    let mut other = external_candidate(0, VideoSource::Wikidata);
    other.recording_mbid = "abcdef01-2345-6789-abcd-0123456789ac".into();
    assert_eq!(
        PgCandidateStore::new(db)
            .admit_external_candidates(std::slice::from_ref(&other))
            .await
            .unwrap(),
        1
    );
    assert!(
        PgCandidateStore::new(db)
            .fresh_links(&[other.recording_mbid], NOW)
            .await
            .unwrap()
            .is_empty(),
        "new recording association requires its own validation"
    );
    let before = states(db).await;
    assert_eq!(
        PgCandidateStore::new(db)
            .admit_external_candidates(&[
                external_candidate(4, VideoSource::MbUrl),
                external_candidate(5, VideoSource::UserSubmission),
            ])
            .await,
        Err(CandidateStoreError::InvalidInput)
    );
    assert_eq!(
        states(db).await,
        before,
        "no partial input admission or moderation bypass"
    );

    db.batch_execute(
        "ALTER TABLE public.video_link_candidates ADD CONSTRAINT fixture_reject_candidate
                      CHECK (video_id <> 'v0000000009')",
    )
    .await
    .unwrap();
    assert_eq!(
        PgCandidateStore::new(db)
            .admit_external_candidates(&[
                external_candidate(8, VideoSource::MbUrl),
                external_candidate(9, VideoSource::MbUrl),
            ])
            .await,
        Err(CandidateStoreError::Store)
    );
    assert_eq!(
        states(db).await,
        before,
        "constraint failure rolls back the whole batch"
    );
    db.batch_execute(
        "ALTER TABLE public.video_link_candidates DROP CONSTRAINT fixture_reject_candidate",
    )
    .await
    .unwrap();
    assert_eq!(
        PgCandidateStore::new(db)
            .admit_external_candidates(&[external_candidate(8, VideoSource::MbUrl)])
            .await
            .unwrap(),
        1,
        "failed transaction leaves connection usable"
    );
}

async fn fresh_link_read_contract(db: &mut Client) {
    reset_candidates(db).await;
    seed(db, 16).await;
    db.execute(
        "UPDATE public.video_link_candidates SET status='validated', title='fixture fresh',
             channel_id='fixture channel', duration_secs=180, embeddable=true,
             validated_at=$1, expires_at=$1::bigint + 100",
        &[&NOW],
    )
    .await
    .unwrap();
    db.execute(
        "UPDATE public.video_link_candidates SET
             status=CASE video_id WHEN 'v0000000003' THEN 'pending' WHEN 'v0000000004' THEN 'rejected'
                    WHEN 'v0000000005' THEN 'expired' ELSE status END,
             validated_at=CASE video_id WHEN 'v0000000001' THEN $1::bigint - $2::bigint
                          WHEN 'v0000000002' THEN $1::bigint + 1 WHEN 'v0000000008' THEN 0 ELSE validated_at END,
             expires_at=CASE WHEN video_id='v0000000000' THEN $1 ELSE expires_at END,
             embeddable=CASE video_id WHEN 'v0000000006' THEN false WHEN 'v0000000007' THEN NULL ELSE embeddable END,
             source=CASE video_id WHEN 'v0000000009' THEN 'wikidata'
                    WHEN 'v0000000015' THEN 'user_submission' ELSE source END",
        &[&NOW, &VALIDATION_TTL_S]).await.unwrap();
    let mbid = "00000000-0000-0000-0000-000000000001".to_string();
    let before = states(db).await;
    let links = PgCandidateStore::new(db)
        .fresh_links(&[mbid.clone(), mbid.clone()], NOW)
        .await
        .unwrap();
    assert_eq!(links.len(), MAX_LINKS_PER_RECORDING as usize);
    assert_eq!(
        links
            .iter()
            .map(|link| link.candidate.video_id.as_str())
            .collect::<Vec<_>>(),
        [
            "v0000000010",
            "v0000000011",
            "v0000000012",
            "v0000000013",
            "v0000000014"
        ]
    );
    assert!(links
        .iter()
        .all(|link| link.title.as_deref() == Some("fixture fresh")
            && link.channel_id.as_deref() == Some("fixture channel")
            && link.duration_secs == Some(180)
            && link.validated_at == NOW
            && link.expires_at == NOW + 100));
    assert_eq!(
        PgCandidateStore::new(db)
            .fresh_links(std::slice::from_ref(&mbid), NOW + 100)
            .await
            .unwrap()
            .len(),
        0,
        "exact expiry is stale even when no purge has run"
    );
    assert_eq!(
        states(db).await,
        before,
        "reads do not replace required metadata cleanup"
    );

    // The last second of TTL is eligible; an overlong expires_at never extends it.
    db.execute("UPDATE public.video_link_candidates SET validated_at=$1, expires_at=$2 WHERE video_id='v0000000014'",
        &[&(NOW - VALIDATION_TTL_S + 1), &i64::MAX]).await.unwrap();
    db.batch_execute("DELETE FROM public.video_link_candidates WHERE video_id <> 'v0000000014'")
        .await
        .unwrap();
    assert_eq!(
        PgCandidateStore::new(db)
            .fresh_links(std::slice::from_ref(&mbid), NOW)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(PgCandidateStore::new(db)
        .fresh_links(std::slice::from_ref(&mbid), NOW + 1)
        .await
        .unwrap()
        .is_empty());
    assert!(PgCandidateStore::new(db)
        .fresh_links(std::slice::from_ref(&mbid), i64::MAX)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        PgCandidateStore::new(db)
            .fresh_links(std::slice::from_ref(&mbid), 0)
            .await,
        Err(CandidateStoreError::InvalidTime)
    );
    assert_eq!(
        PgCandidateStore::new(db)
            .fresh_links(std::slice::from_ref(&mbid), i64::MIN)
            .await,
        Err(CandidateStoreError::InvalidTime)
    );
    assert_eq!(
        PgCandidateStore::new(db)
            .fresh_links(&["invalid".into()], NOW)
            .await,
        Err(CandidateStoreError::InvalidInput)
    );
    assert_eq!(
        PgCandidateStore::new(db)
            .fresh_links(&vec![mbid; MAX_RECORDINGS_PER_READ + 1], NOW)
            .await,
        Err(CandidateStoreError::InvalidInput)
    );
    assert!(PgCandidateStore::new(db)
        .fresh_links(&[], NOW)
        .await
        .unwrap()
        .is_empty());

    reset_candidates(db).await;
    // Maximum read fan-out: five links per requested recording, never the sixth.
    db.execute(
        "INSERT INTO public.video_link_candidates
         (recording_mbid, video_id, source, status, embeddable, validated_at, expires_at)
         SELECT ('00000000-0000-0000-0000-' || lpad(r::text,12,'0'))::uuid,
                'v' || lpad(v::text,10,'0'), 'mb_url', 'validated', true, $1, $1::bigint + 100
         FROM generate_series(1,50) r CROSS JOIN generate_series(0,5) v",
        &[&NOW],
    )
    .await
    .unwrap();
    let mbids: Vec<_> = (1..=50)
        .map(|i| format!("00000000-0000-0000-0000-{i:012}"))
        .collect();
    let links = PgCandidateStore::new(db)
        .fresh_links(&mbids, NOW)
        .await
        .unwrap();
    assert_eq!(
        links.len(),
        MAX_RECORDINGS_PER_READ * MAX_LINKS_PER_RECORDING as usize
    );
    assert!(links
        .iter()
        .all(|link| link.candidate.video_id != "v0000000005"));
}

async fn mirror_harvest_contract(db: &mut Client, config: &Config) {
    reset_candidates(db).await;
    // Reader-column fixtures of W6b, not a staging apply or full mirror migration.
    db.batch_execute(
        "CREATE TABLE public.mb_url (mbid uuid PRIMARY KEY, url text NOT NULL);
         CREATE TABLE public.mb_recording_url (
             recording_mbid uuid NOT NULL, url_mbid uuid NOT NULL REFERENCES public.mb_url(mbid),
             link_name text NOT NULL, is_video boolean NOT NULL,
             PRIMARY KEY (recording_mbid, url_mbid, link_name));
         CREATE TABLE public.mb_recording_music_video (
             recording_a_mbid uuid NOT NULL, recording_b_mbid uuid NOT NULL,
             PRIMARY KEY (recording_a_mbid, recording_b_mbid),
             CHECK (recording_a_mbid < recording_b_mbid));
         INSERT INTO public.mb_url
         SELECT ('10000000-0000-0000-0000-' || lpad(i::text, 12, '0'))::uuid,
                CASE i WHEN 1 THEN 'https://youtu.be/v0000000000'
                       WHEN 2 THEN 'https://www.youtube.com/watch?v=v0000000001'
                       WHEN 3 THEN 'https://other.example.test/watch?v=v0000000002'
                       WHEN 4 THEN 'https://youtu.be/invalid'
                       WHEN 5 THEN 'https://youtu.be/v0000000003?' || repeat('x', 2048)
                       ELSE 'https://youtu.be/v0000000004' END
         FROM generate_series(1, 6) AS i;
         INSERT INTO public.mb_recording_url
         SELECT CASE i WHEN 2 THEN '00000000-0000-0000-0000-000000000002'::uuid
                       ELSE '00000000-0000-0000-0000-000000000001'::uuid END,
                mbid, 'stream', i <> 6
         FROM public.mb_url u
         JOIN generate_series(1, 6) AS i
           ON u.mbid = ('10000000-0000-0000-0000-' || lpad(i::text, 12, '0'))::uuid;
         INSERT INTO public.mb_recording_url VALUES (
             '00000000-0000-0000-0000-000000000001',
             '10000000-0000-0000-0000-000000000001', 'duplicate relation', true);
         INSERT INTO public.mb_recording_music_video VALUES (
             '00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000002')",
    )
    .await
    .unwrap();
    let a = "00000000-0000-0000-0000-000000000001".to_owned();
    let b = "00000000-0000-0000-0000-000000000002".to_owned();
    let missing = "00000000-0000-0000-0000-000000000003".to_owned();
    let selected = [b.clone(), a.clone(), missing.clone(), a.clone()];
    let page = PgMirrorSource::new(db)
        .candidate_page(&selected, None)
        .await
        .unwrap();
    assert_eq!(page.scanned_rows, 10);
    assert_eq!(page.next_cursor, None);
    assert_eq!(
        page.candidates,
        [a.clone(), b.clone()]
            .into_iter()
            .flat_map(
                |recording_mbid| ["v0000000000", "v0000000001"].map(|video_id| Candidate {
                    recording_mbid: recording_mbid.clone(),
                    video_id: video_id.into(),
                    source: VideoSource::MbUrl,
                })
            )
            .collect::<Vec<_>>()
    );
    assert!(PgMirrorSource::new(db)
        .candidate_page(&[missing], None)
        .await
        .unwrap()
        .candidates
        .is_empty());
    assert!(PgMirrorSource::new(db)
        .candidate_page(&[], None)
        .await
        .unwrap()
        .candidates
        .is_empty());
    assert!(
        states(db).await.is_empty(),
        "mirror reads never admit app candidates"
    );

    db.batch_execute(
        "DELETE FROM public.mb_recording_music_video;
         DELETE FROM public.mb_recording_url;
         DELETE FROM public.mb_url;
         INSERT INTO public.mb_url
         SELECT ('10000000-0000-0000-0000-' || lpad(i::text, 12, '0'))::uuid,
                CASE WHEN i <= 200 THEN 'https://other.example.test/not-a-video'
                     ELSE 'https://youtu.be/v0000000000' END
         FROM generate_series(1, 202) AS i;
         INSERT INTO public.mb_recording_url
         SELECT CASE WHEN mbid = '10000000-0000-0000-0000-000000000202'
                     THEN '00000000-0000-0000-0000-000000000002'::uuid
                     ELSE '00000000-0000-0000-0000-000000000001'::uuid END,
                mbid, 'stream', true FROM public.mb_url",
    )
    .await
    .unwrap();
    let first = PgMirrorSource::new(db)
        .candidate_page(&selected, None)
        .await
        .unwrap();
    assert_eq!(first.scanned_rows, MAX_MIRROR_ROWS_PER_PAGE);
    assert!(first.candidates.is_empty());
    assert_eq!(
        first.next_cursor,
        Some(MirrorCursor {
            recording_mbid: a.clone(),
            url_mbid: "10000000-0000-0000-0000-000000000200".into(),
        })
    );
    let last = PgMirrorSource::new(db)
        .candidate_page(&selected, first.next_cursor.as_ref())
        .await
        .unwrap();
    assert_eq!(last.scanned_rows, 2);
    assert_eq!(last.candidates.len(), 2);
    assert_eq!(last.candidates[0].recording_mbid, a);
    assert_eq!(last.candidates[1].recording_mbid, b);
    assert_eq!(last.next_cursor, None);
    let replay = PgMirrorSource::new(db)
        .candidate_page(&selected, first.next_cursor.as_ref())
        .await
        .unwrap();
    assert_eq!(last, replay);

    let mut locker = connect(config).await;
    let lock = locker.transaction().await.unwrap();
    lock.batch_execute("LOCK TABLE public.mb_recording_url IN ACCESS EXCLUSIVE MODE")
        .await
        .unwrap();
    let started = std::time::Instant::now();
    assert_eq!(
        PgMirrorSource::new(db)
            .candidate_page(&selected, None)
            .await,
        Err(HarvestError::Store)
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    let mut app = connect(config).await;
    let failure = admit_mirror_pages(
        &mut PgMirrorSource::new(db),
        &mut PgCandidateStore::new(&mut app),
        &selected,
        first.next_cursor.as_ref(),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.error, MirrorHarvestError::Mirror);
    assert_eq!(failure.progress.committed_pages, 0);
    assert_eq!(failure.progress.next_cursor, first.next_cursor);
    assert!(!failure.progress.complete);
    assert!(states(&app).await.is_empty());
    // Empty/invalid selections never touch the locked source or app tables.
    assert!(
        admit_mirror_pages(
            &mut PgMirrorSource::new(db),
            &mut PgCandidateStore::new(&mut app),
            &[],
            None,
        )
        .await
        .unwrap()
        .complete
    );
    assert_eq!(
        admit_mirror_pages(
            &mut PgMirrorSource::new(db),
            &mut PgCandidateStore::new(&mut app),
            &["invalid".into()],
            None,
        )
        .await
        .unwrap_err()
        .error,
        MirrorHarvestError::InvalidInput
    );
    lock.rollback().await.unwrap();
    assert_eq!(
        PgMirrorSource::new(db)
            .candidate_page(&selected, None)
            .await
            .unwrap(),
        first
    );
}

async fn mirror_admission_contract(db: &mut Client, config: &Config) {
    reset_candidates(db).await;
    db.batch_execute(
        "DELETE FROM public.mb_recording_music_video;
         DELETE FROM public.mb_recording_url;
         DELETE FROM public.mb_url;
         INSERT INTO public.mb_url
         SELECT ('10000000-0000-0000-0000-' || lpad(i::text, 12, '0'))::uuid,
                CASE WHEN i <= 200 THEN 'https://other.example.test/not-a-video'
                     ELSE 'https://youtu.be/v' || lpad(i::text, 10, '0') END
         FROM generate_series(1, 403) AS i;
         INSERT INTO public.mb_recording_url
         SELECT '00000000-0000-0000-0000-000000000001', mbid, label, true
         FROM public.mb_url CROSS JOIN unnest(ARRAY['stream', 'duplicate']) AS label;
         INSERT INTO public.mb_recording_music_video VALUES (
             '00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000002')",
    )
    .await
    .unwrap();
    let a = "00000000-0000-0000-0000-000000000001".to_owned();
    let b = "00000000-0000-0000-0000-000000000002".to_owned();
    let selected = [b.clone(), a.clone(), a.clone()];
    let mut mirror = connect(config).await;
    let first = admit_mirror_pages(
        &mut PgMirrorSource::new(&mut mirror),
        &mut PgCandidateStore::new(db),
        &selected,
        None,
    )
    .await
    .unwrap();
    assert_eq!(first.committed_pages, MAX_MIRROR_PAGES_PER_RUN);
    assert_eq!(first.scanned_rows, 800);
    assert_eq!(first.candidate_pairs, 400);
    assert_eq!(first.changed_rows, 400);
    assert!(!first.complete);
    assert_eq!(
        first.next_cursor,
        Some(MirrorCursor {
            recording_mbid: b.clone(),
            url_mbid: "10000000-0000-0000-0000-000000000397".into(),
        })
    );
    let last = admit_mirror_pages(
        &mut PgMirrorSource::new(&mut mirror),
        &mut PgCandidateStore::new(db),
        &selected,
        first.next_cursor.as_ref(),
    )
    .await
    .unwrap();
    assert!(last.complete);
    assert_eq!(last.next_cursor, None);
    assert_eq!(last.committed_pages, 1);
    assert_eq!(last.scanned_rows, 6);
    assert_eq!(last.candidate_pairs, 6);
    assert_eq!(last.changed_rows, 6);
    assert_eq!(states(db).await.len(), 406);
    let replay = admit_mirror_pages(
        &mut PgMirrorSource::new(&mut mirror),
        &mut PgCandidateStore::new(db),
        &selected,
        None,
    )
    .await
    .unwrap();
    assert_eq!(replay.changed_rows, 0);
    assert_eq!(replay.next_cursor, first.next_cursor);
    assert_eq!(states(db).await.len(), 406);

    reset_candidates(db).await;
    db.batch_execute(
        "ALTER TABLE public.video_link_candidates ADD CONSTRAINT fail_paired_admission
         CHECK (recording_mbid <> '00000000-0000-0000-0000-000000000002'::uuid)",
    )
    .await
    .unwrap();
    let failure = admit_mirror_pages(
        &mut PgMirrorSource::new(&mut mirror),
        &mut PgCandidateStore::new(db),
        &selected,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(failure.error, MirrorHarvestError::Admission);
    assert_eq!(failure.progress.committed_pages, 3);
    assert_eq!(failure.progress.scanned_rows, 600);
    assert_eq!(failure.progress.candidate_pairs, 203);
    assert_eq!(failure.progress.changed_rows, 203);
    assert!(!failure.progress.complete);
    assert_eq!(
        failure.progress.next_cursor,
        Some(MirrorCursor {
            recording_mbid: b,
            url_mbid: "10000000-0000-0000-0000-000000000197".into(),
        })
    );
    let rows = states(db).await;
    assert_eq!(rows.len(), 203, "earlier app commits survive a failed page");
    assert!(rows.iter().all(|(_, status, _, _, _)| status == "pending"));
    db.batch_execute(
        "ALTER TABLE public.video_link_candidates DROP CONSTRAINT fail_paired_admission",
    )
    .await
    .unwrap();
    let resumed = admit_mirror_pages(
        &mut PgMirrorSource::new(&mut mirror),
        &mut PgCandidateStore::new(db),
        &selected,
        failure.progress.next_cursor.as_ref(),
    )
    .await
    .unwrap();
    assert!(resumed.complete);
    assert_eq!(resumed.committed_pages, 2);
    assert_eq!(resumed.scanned_rows, 206);
    assert_eq!(resumed.changed_rows, 203);
    assert_eq!(states(db).await.len(), 406);
}

async fn submission_store_contract(db: &mut Client, config: &Config) {
    reset_candidates(db).await;
    db.batch_execute("DELETE FROM public.video_user_submissions")
        .await
        .unwrap();
    let mbid = "ABCDEF01-2345-6789-ABCD-0123456789AB";
    let mut store = PgSubmissionStore::new(db);
    assert_eq!(
        store.submit(mbid, "invalid", "fixture-user").await,
        Err(SubmissionStoreError::InvalidInput)
    );
    assert!(store.pending().await.unwrap().is_empty());
    let approved = store
        .submit(mbid, "https://youtu.be/v0000000000", "fixture-user")
        .await
        .unwrap();
    let rejected = store
        .submit(mbid, "v0000000001", "other-user")
        .await
        .unwrap();
    let queued = store.pending().await.unwrap();
    assert_eq!(queued.len(), 2);
    assert_eq!(queued[0].id, approved);
    assert_eq!(queued[0].recording_mbid, mbid.to_ascii_lowercase());
    assert_eq!(queued[0].video_id, "v0000000000");
    assert_eq!(queued[0].submitted_by, "fixture-user");
    assert!(
        states(db).await.is_empty(),
        "pending submissions never promote"
    );
    let mut store = PgSubmissionStore::new(db);
    assert_eq!(
        store.decide(&approved, true, Some(" ")).await,
        Err(SubmissionStoreError::InvalidInput)
    );
    assert_eq!(
        store
            .decide(
                &approved.to_ascii_uppercase(),
                true,
                Some("matched recording")
            )
            .await,
        Ok(SubmissionStatus::Approved)
    );
    assert_eq!(
        store
            .decide(&rejected, false, Some("wrong recording"))
            .await,
        Ok(SubmissionStatus::Rejected)
    );
    for id in [&approved, &rejected, "00000000-0000-0000-0000-000000000099"] {
        for approve in [true, false] {
            assert_eq!(
                store.decide(id, approve, Some("must not overwrite")).await,
                Err(SubmissionStoreError::NotPending)
            );
        }
    }
    assert!(store.pending().await.unwrap().is_empty());
    let rows = states(db).await;
    assert_eq!(rows.len(), 1, "rejection does not promote");
    assert_eq!(rows[0].0, "v0000000000");
    assert_eq!(rows[0].1, "pending");
    assert_eq!((rows[0].2.clone(), rows[0].3, rows[0].4), (None, 0, 0));
    let row = db
        .query_one("SELECT source FROM public.video_link_candidates", &[])
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "user_submission");
    let row = db
        .query_one(
            "SELECT status, reason, decided_at IS NOT NULL FROM public.video_user_submissions
         WHERE id = $1::text::uuid",
            &[&approved],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "approved");
    assert_eq!(row.get::<_, String>(1), "matched recording");
    assert!(row.get::<_, bool>(2));

    db.batch_execute(
        "UPDATE public.video_link_candidates SET source = 'mb_url', status = 'validated',
         title = 'retained title', channel_id = 'retained channel', duration_secs = 123,
         embeddable = true, validated_at = 1700000000, expires_at = 1702592000",
    )
    .await
    .unwrap();
    let before = db
        .query_one(
            "SELECT row_to_json(c)::text FROM public.video_link_candidates c",
            &[],
        )
        .await
        .unwrap()
        .get::<_, String>(0);
    let mut store = PgSubmissionStore::new(db);
    let duplicate = store
        .submit(mbid, "v0000000000", "fixture-user")
        .await
        .unwrap();
    store.decide(&duplicate, true, None).await.unwrap();
    let after = db
        .query_one(
            "SELECT row_to_json(c)::text FROM public.video_link_candidates c",
            &[],
        )
        .await
        .unwrap()
        .get::<_, String>(0);
    assert_eq!(
        before, after,
        "approval cannot reset provenance, validation, TTL or timestamps"
    );

    let failed = PgSubmissionStore::new(db)
        .submit(mbid, "v0000000007", "fixture-user")
        .await
        .unwrap();
    db.batch_execute(
        "ALTER TABLE public.video_link_candidates ADD CONSTRAINT refuse_submission
         CHECK (video_id <> 'v0000000007')",
    )
    .await
    .unwrap();
    assert_eq!(
        PgSubmissionStore::new(db)
            .decide(&failed, true, Some("must roll back"))
            .await,
        Err(SubmissionStoreError::Store)
    );
    let row = db
        .query_one(
            "SELECT status, reason IS NULL, decided_at IS NULL
         FROM public.video_user_submissions WHERE id = $1::text::uuid",
            &[&failed],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "pending");
    assert!(row.get::<_, bool>(1));
    assert!(row.get::<_, bool>(2));
    assert_eq!(states(db).await.len(), 1);
    db.batch_execute("ALTER TABLE public.video_link_candidates DROP CONSTRAINT refuse_submission")
        .await
        .unwrap();
    PgSubmissionStore::new(db)
        .decide(&failed, true, None)
        .await
        .unwrap();
    assert_eq!(states(db).await.len(), 2);

    let contested = PgSubmissionStore::new(db)
        .submit(mbid, "v0000000008", "fixture-user")
        .await
        .unwrap();
    let mut competitor = connect(config).await;
    let other_id = contested.clone();
    let other = tokio::spawn(async move {
        PgSubmissionStore::new(&mut competitor)
            .decide(&other_id, false, None)
            .await
    });
    let first = PgSubmissionStore::new(db)
        .decide(&contested, true, None)
        .await;
    let second = other.await.unwrap();
    assert!(
        matches!(
            (first, second),
            (
                Ok(SubmissionStatus::Approved),
                Err(SubmissionStoreError::NotPending)
            ) | (
                Err(SubmissionStoreError::NotPending),
                Ok(SubmissionStatus::Rejected)
            )
        ),
        "exactly one concurrent decision wins"
    );
    let row = db
        .query_one(
            "SELECT status = 'approved', EXISTS (
             SELECT 1 FROM public.video_link_candidates WHERE video_id = 'v0000000008'
         ) FROM public.video_user_submissions WHERE id = $1::text::uuid",
            &[&contested],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, bool>(0), row.get::<_, bool>(1));

    let locked = PgSubmissionStore::new(db)
        .submit(mbid, "v0000000009", "fixture-user")
        .await
        .unwrap();
    let mut locker = connect(config).await;
    let lock = locker.transaction().await.unwrap();
    lock.query_one(
        "SELECT id FROM public.video_user_submissions WHERE id = $1::text::uuid FOR UPDATE",
        &[&locked],
    )
    .await
    .unwrap();
    let started = std::time::Instant::now();
    assert_eq!(
        PgSubmissionStore::new(db).decide(&locked, true, None).await,
        Err(SubmissionStoreError::Store)
    );
    assert!(started.elapsed() < Duration::from_secs(4));
    lock.rollback().await.unwrap();
    assert_eq!(
        PgSubmissionStore::new(db).pending().await.unwrap()[0].id,
        locked
    );
    PgSubmissionStore::new(db)
        .decide(&locked, false, None)
        .await
        .unwrap();

    db.batch_execute(
        "INSERT INTO public.video_user_submissions
         (id, recording_mbid, video_id, submitted_by, created_at)
         SELECT ('20000000-0000-0000-0000-' || lpad(i::text, 12, '0'))::uuid,
                'abcdef01-2345-6789-abcd-0123456789ab', 'v0000000000', 'fixture-user',
                '2020-01-01'::timestamptz FROM generate_series(1, 55) AS i",
    )
    .await
    .unwrap();
    let mut store = PgSubmissionStore::new(db);
    let queue = store.pending().await.unwrap();
    assert_eq!(queue.len(), MAX_PENDING_SUBMISSIONS as usize);
    assert_eq!(queue[0].id, "20000000-0000-0000-0000-000000000001");
    assert_eq!(queue[49].id, "20000000-0000-0000-0000-000000000050");
    store.decide(&queue[0].id, false, None).await.unwrap();
    let queue = store.pending().await.unwrap();
    assert_eq!(queue.len(), MAX_PENDING_SUBMISSIONS as usize);
    assert_eq!(queue[0].id, "20000000-0000-0000-0000-000000000002");
    assert_eq!(queue[49].id, "20000000-0000-0000-0000-000000000051");
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
                         AND to_regclass('public.video_coverage_snapshots') IS NULL
                         AND to_regclass('public.mb_url') IS NULL
                         AND to_regclass('public.mb_recording_url') IS NULL
                         AND to_regclass('public.mb_recording_music_video') IS NULL",
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
                candidate_admission_contract(&mut db).await;
                fresh_link_read_contract(&mut db).await;
                mirror_harvest_contract(&mut db, &config).await;
                mirror_admission_contract(&mut db, &config).await;
                submission_store_contract(&mut db, &config).await;
            })
            .await
            .expect("bounded PostgreSQL acceptance");
        });
}
