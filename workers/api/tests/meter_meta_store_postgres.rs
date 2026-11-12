//! Production metadata adapter against independently migrated app and mirror
//! stores. CI prepares disposable databases with each real migration history;
//! the separation test guards against cross-store relations.
use nntune_api::handlers::{Store, StoreError};
use nntune_api::worker_entry::catalogue_store_for_postgres_test;
use std::cell::Cell;
use std::rc::Rc;
use tokio_postgres::{Client, Config, NoTls};

const USER: &str = "u1";
const LOCAL_TRACK: &str = "22222222-2222-2222-2222-222222222222";
const SHORT_RECORDING: &str = "33333333-3333-3333-3333-333333333333";
const NOW: u64 = 1_700_000_000;

async fn connect(var: &str, dbname: &str) -> Client {
    let config: Config = std::env::var(var)
        .expect("CI migrated store is required; never silently skip")
        .parse()
        .unwrap();
    assert_eq!(config.get_dbname(), Some(dbname));
    assert!(config.get_hostaddrs().is_empty(), "no address overrides");
    assert!(
        matches!(config.get_hosts(), [tokio_postgres::config::Host::Tcp(host)]
        if matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1"))
    );
    let (client, connection) = config.connect(NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    client
}

async fn app() -> Client {
    connect("NNTUNE_TEST_APP_DATABASE_URL", "nntune_app_ci").await
}

async fn mirror() -> Client {
    connect("NNTUNE_TEST_MIRROR_DATABASE_URL", "nntune_mirror_ci").await
}

async fn has_table(client: &Client, table: &str) -> bool {
    let row = client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM pg_tables WHERE schemaname = 'public' AND tablename = $1)",
            &[&table],
        )
        .await
        .unwrap();
    row.get(0)
}

async fn has_relation(client: &Client, name: &str) -> bool {
    let row = client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'public' AND c.relname = $1)",
            &[&name],
        )
        .await
        .unwrap();
    row.get(0)
}

/// The stores are independently migrated: neither side names the other's
/// relations, so the split query boundary is a schema fact, not a fixture
/// claim.
#[tokio::test]
async fn migrated_stores_have_no_cross_side_relations() {
    let app = app().await;
    for table in ["tracks", "track_media", "cache_objects"] {
        assert!(has_table(&app, table).await, "app store misses {table}");
    }
    assert!(
        !has_relation(&app, "mb_recording").await,
        "app store must not name mb_recording"
    );
    let mirror = mirror().await;
    assert!(
        has_table(&mirror, "mb_recording").await,
        "mirror store misses mb_recording"
    );
    for table in ["tracks", "track_media", "cache_objects"] {
        assert!(
            !has_relation(&mirror, table).await,
            "mirror store must not name {table}"
        );
    }
}

// Tests share the migrated databases and run concurrently. Bare conflict
// handling covers every unique index when identical seeds race.
async fn seed_local(app: &Client) {
    app.batch_execute(&format!(
        "INSERT INTO users (id, email, created_at) VALUES ('{USER}', 'u1@ci.invalid', 1700000000) \
         ON CONFLICT DO NOTHING; \
         INSERT INTO ingest_jobs (id, user_id, idempotency_key, status) \
         VALUES ('job-meter-1', '{USER}', 'meter-1', 'done') ON CONFLICT DO NOTHING; \
         INSERT INTO tracks (id, mbid, release_id, title, medium_no, track_no, duration_ms) \
         VALUES ('{LOCAL_TRACK}', '{SHORT_RECORDING}', '88888888-8888-8888-8888-888888888888', 'Track', 1, 1, 60000) \
         ON CONFLICT DO NOTHING"
    )).await.unwrap();
}

async fn seed_media(app: &Client) {
    let hash = "a".repeat(64);
    app.batch_execute(&format!(
        "INSERT INTO cache_objects (content_hash, byte_size, r2_bucket, r2_key, last_verified_at) \
         VALUES ('{hash}', 2000000, 'ci-bucket', 'ci/key', 1700000100) ON CONFLICT DO NOTHING; \
         INSERT INTO track_media (user_id, track_id, source_job_id, content_hash, format, bitrate, verified_at) \
         VALUES ('{USER}', '{LOCAL_TRACK}', 'job-meter-1', '{hash}', 'flac', 128, 1700000200) \
         ON CONFLICT DO NOTHING"
    )).await.unwrap();
}

fn meter_store(app: Client, mirror: Result<Client, StoreError>) -> (impl Store, Rc<Cell<usize>>) {
    let attempts = Rc::new(Cell::new(0));
    let store = catalogue_store_for_postgres_test(app, true, mirror, attempts.clone());
    (store, attempts)
}

#[tokio::test]
async fn local_uuid_resolves_app_and_mirror_rungs() {
    let app = app().await;
    seed_local(&app).await;
    seed_media(&app).await;
    let mirror = mirror().await;
    mirror.batch_execute(&format!(
        "INSERT INTO mb_recording (mbid, title, duration_ms) VALUES ('{SHORT_RECORDING}', 'Rec', 61000) ON CONFLICT DO NOTHING"
    )).await.unwrap();
    let (store, attempts) = meter_store(app, Ok(mirror));
    let meta = store.track_meter_meta(USER, LOCAL_TRACK).await.unwrap();
    assert_eq!(attempts.get(), 1);
    assert_eq!(meta.track_duration_ms, Some(60_000));
    assert_eq!(meta.mb_duration_ms, Some(61_000));
    assert_eq!(meta.bitrate_kbps, Some(128));
    assert_eq!(meta.file_bytes, Some(2_000_000));
}

#[tokio::test]
async fn recording_mbid_resolves_artifact_and_short_mirror() {
    let app = app().await;
    seed_local(&app).await;
    seed_media(&app).await;
    let mirror = mirror().await;
    mirror.batch_execute(&format!(
        "INSERT INTO mb_recording (mbid, title, duration_ms) VALUES ('{SHORT_RECORDING}', 'Rec', 61000) ON CONFLICT DO NOTHING"
    )).await.unwrap();
    let (store, attempts) = meter_store(app, Ok(mirror));
    let meta = store.track_meter_meta(USER, SHORT_RECORDING).await.unwrap();
    assert_eq!(attempts.get(), 1);
    assert_eq!(meta.track_duration_ms, Some(60_000));
    assert_eq!(meta.mb_duration_ms, Some(61_000));
    assert_eq!(meta.bitrate_kbps, Some(128));
    assert_eq!(meta.file_bytes, Some(2_000_000));
    let duration = nntune_api::ledger::resolve_duration_ms(
        meta.track_duration_ms,
        meta.mb_duration_ms,
        meta.bitrate_kbps,
        meta.file_bytes,
    );
    assert_eq!(duration, Some(60_000));
    assert_eq!(
        nntune_api::playback::continuation_until(NOW, duration),
        NOW + 60 + nntune_api::playback::LEASE_TTL_SECS
    );
}

#[tokio::test]
async fn recording_mbid_without_artifact_uses_mirror_only() {
    let recording = "77777777-7777-7777-7777-777777777777";
    let mirror = mirror().await;
    mirror.batch_execute(&format!(
        "INSERT INTO mb_recording (mbid, title, duration_ms) VALUES ('{recording}', 'Orphan', 61000) ON CONFLICT DO NOTHING"
    )).await.unwrap();
    let (store, attempts) = meter_store(app().await, Ok(mirror));
    let meta = store.track_meter_meta(USER, recording).await.unwrap();
    assert_eq!(attempts.get(), 1);
    assert_eq!(meta.track_duration_ms, None);
    assert_eq!(meta.mb_duration_ms, Some(61_000));
    assert_eq!(meta.bitrate_kbps, None);
    assert_eq!(meta.file_bytes, None);
    assert_eq!(
        nntune_api::ledger::resolve_duration_ms(
            meta.track_duration_ms,
            meta.mb_duration_ms,
            meta.bitrate_kbps,
            meta.file_bytes,
        ),
        Some(61_000)
    );
}

#[tokio::test]
async fn long_recording_never_falls_back_to_900() {
    let recording = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
    let app = app().await;
    seed_local(&app).await;
    app.batch_execute(&format!(
        "INSERT INTO tracks (id, mbid, release_id, title, medium_no, track_no, duration_ms) \
         VALUES ('99999999-9999-9999-9999-999999999999', '{recording}', \
         '88888888-8888-8888-8888-888888888888', 'Long', 1, 2, NULL) ON CONFLICT DO NOTHING"
    ))
    .await
    .unwrap();
    let mirror = mirror().await;
    mirror.batch_execute(&format!(
        "INSERT INTO mb_recording (mbid, title, duration_ms) VALUES ('{recording}', 'Long', 1500000) ON CONFLICT DO NOTHING"
    )).await.unwrap();
    let (store, attempts) = meter_store(app, Ok(mirror));
    let meta = store.track_meter_meta(USER, recording).await.unwrap();
    assert_eq!(attempts.get(), 1);
    assert_eq!(meta.track_duration_ms, None);
    assert_eq!(meta.mb_duration_ms, Some(1_500_000));
    assert_eq!(meta.bitrate_kbps, None);
    assert_eq!(meta.file_bytes, None);
    let duration = nntune_api::ledger::resolve_duration_ms(
        meta.track_duration_ms,
        meta.mb_duration_ms,
        meta.bitrate_kbps,
        meta.file_bytes,
    );
    assert_eq!(duration, Some(1_500_000));
    assert_eq!(
        nntune_api::playback::continuation_until(NOW, duration),
        NOW + 1500 + nntune_api::playback::LEASE_TTL_SECS
    );
}

#[tokio::test]
async fn shared_recording_suppresses_artifacts_but_exact_occurrences_do_not() {
    let recording = "10000000-0000-0000-0000-000000000001";
    let first = "10000000-0000-0000-0000-000000000002";
    let second = "10000000-0000-0000-0000-000000000003";
    let app_client = app().await;
    seed_local(&app_client).await;
    app_client.batch_execute(&format!(
        "INSERT INTO tracks (id, mbid, release_id, title, medium_no, track_no, duration_ms) VALUES \
         ('{second}', '{recording}', '10000000-0000-0000-0000-000000000004', 'Second', 1, 2, 240000), \
         ('{first}', '{recording}', '10000000-0000-0000-0000-000000000004', 'First', 1, 1, 60000) ON CONFLICT DO NOTHING; \
         INSERT INTO cache_objects (content_hash, byte_size, r2_bucket, r2_key, last_verified_at) VALUES \
         (repeat('d', 64), 1000000, 'ci-bucket', 'ci/first', 1700000100), \
         (repeat('e', 64), 3000000, 'ci-bucket', 'ci/second', 1700000100) ON CONFLICT DO NOTHING; \
         INSERT INTO track_media (user_id, track_id, source_job_id, content_hash, format, bitrate, verified_at) VALUES \
         ('{USER}', '{first}', 'job-meter-1', repeat('d', 64), 'flac', 64, 1700000200), \
         ('{USER}', '{second}', 'job-meter-1', repeat('e', 64), 'flac', 192, 1700000200) ON CONFLICT DO NOTHING"
    )).await.unwrap();
    let mirror_client = mirror().await;
    mirror_client.batch_execute(&format!(
        "INSERT INTO mb_recording (mbid, title, duration_ms) VALUES ('{recording}', 'Shared', 120000) ON CONFLICT DO NOTHING"
    )).await.unwrap();
    drop(app_client);
    drop(mirror_client);
    for (id, duration, bitrate, bytes) in [
        (recording, None, None, None),
        (first, Some(60_000), Some(64), Some(1_000_000)),
        (second, Some(240_000), Some(192), Some(3_000_000)),
    ] {
        let (store, attempts) = meter_store(app().await, Ok(mirror().await));
        let meta = store.track_meter_meta(USER, id).await.unwrap();
        assert_eq!(attempts.get(), 1);
        assert_eq!(meta.track_duration_ms, duration);
        assert_eq!(meta.mb_duration_ms, Some(120_000));
        assert_eq!(meta.bitrate_kbps, bitrate);
        assert_eq!(meta.file_bytes, bytes);
        if id == recording {
            let duration = nntune_api::ledger::resolve_duration_ms(
                meta.track_duration_ms,
                meta.mb_duration_ms,
                meta.bitrate_kbps,
                meta.file_bytes,
            );
            assert_eq!(duration, Some(120_000));
            assert_eq!(
                nntune_api::playback::continuation_until(NOW, duration),
                NOW + 120 + nntune_api::playback::LEASE_TTL_SECS
            );
        }
    }
}

#[tokio::test]
async fn exact_local_id_wins_recording_namespace_collision() {
    let local = "20000000-0000-0000-0000-000000000001";
    let recording = "20000000-0000-0000-0000-000000000002";
    let app = app().await;
    app.batch_execute(&format!(
        "INSERT INTO tracks (id, mbid, release_id, title, medium_no, track_no, duration_ms) VALUES \
         ('{local}', '{recording}', '20000000-0000-0000-0000-000000000004', 'Local', 1, 1, 60000), \
         ('20000000-0000-0000-0000-000000000003', '{local}', '20000000-0000-0000-0000-000000000004', 'Collision', 1, 2, 999000) ON CONFLICT DO NOTHING"
    )).await.unwrap();
    let mirror = mirror().await;
    mirror.batch_execute(&format!(
        "INSERT INTO mb_recording (mbid, title, duration_ms) VALUES \
         ('{recording}', 'Local recording', 61000), ('{local}', 'Collision recording', 999000) ON CONFLICT DO NOTHING"
    )).await.unwrap();
    let (store, attempts) = meter_store(app, Ok(mirror));
    let meta = store.track_meter_meta(USER, local).await.unwrap();
    assert_eq!(attempts.get(), 1);
    assert_eq!(meta.track_duration_ms, Some(60_000));
    assert_eq!(meta.mb_duration_ms, Some(61_000));
    assert_eq!(meta.bitrate_kbps, None);
    assert_eq!(meta.file_bytes, None);
}

#[tokio::test]
async fn unconfigured_mirror_degrades_without_attempt() {
    let app = app().await;
    seed_local(&app).await;
    let attempts = Rc::new(Cell::new(0));
    let store = catalogue_store_for_postgres_test(app, false, Err(StoreError), attempts.clone());
    let meta = store.track_meter_meta(USER, LOCAL_TRACK).await.unwrap();
    assert_eq!(attempts.get(), 0);
    assert_eq!(meta.track_duration_ms, Some(60_000));
    assert_eq!(meta.mb_duration_ms, None);
}

#[tokio::test]
async fn configured_mirror_failure_never_degrades() {
    let app = app().await;
    seed_local(&app).await;
    let (store, attempts) = meter_store(app, Err(StoreError));
    assert!(store.track_meter_meta(USER, LOCAL_TRACK).await.is_err());
    assert_eq!(attempts.get(), 1);
}

#[tokio::test]
async fn non_uuid_needs_no_database() {
    let app = app().await;
    seed_local(&app).await;
    let attempts = Rc::new(Cell::new(0));
    let store = catalogue_store_for_postgres_test(app, false, Err(StoreError), attempts.clone());
    let meta = store.track_meter_meta(USER, "track-9").await.unwrap();
    assert_eq!(meta, nntune_api::meter_sink::TrackMeterMeta::default());
    assert_eq!(attempts.get(), 0);
    // The injected app client is single-use: a valid lookup proves the
    // non-UUID short circuit did not consume it.
    let local = store.track_meter_meta(USER, LOCAL_TRACK).await.unwrap();
    assert_eq!(local.track_duration_ms, Some(60_000));
}

#[tokio::test]
async fn unknown_id_falls_back_without_error() {
    let (store, attempts) = meter_store(app().await, Ok(mirror().await));
    let meta = store
        .track_meter_meta(USER, "66666666-6666-6666-6666-666666666666")
        .await
        .unwrap();
    assert_eq!(meta, nntune_api::meter_sink::TrackMeterMeta::default());
    assert_eq!(attempts.get(), 1);
    let duration = nntune_api::ledger::resolve_duration_ms(
        meta.track_duration_ms,
        meta.mb_duration_ms,
        meta.bitrate_kbps,
        meta.file_bytes,
    );
    assert_eq!(duration, None);
    assert_eq!(nntune_api::playback::continuation_until(NOW, duration), NOW + 900);
}
