//! Production `track_meter_meta` against independently migrated app and
//! mirror stores. CI's `prepare_migrated_stores.sh` creates two empty
//! disposable databases and applies each side's real migration history from
//! `migrations/manifest.toml` (app-target to `nntune_app_ci`, mirror-target
//! to `nntune_mirror_ci`), in manifest order. The app connection therefore
//! carries the migrated DDL for `tracks` / `track_media` / `cache_objects`
//! (with FKs, NOT NULL columns and CHECKs from `0022_library_artifacts.sql`)
//! and no `mb_recording`; the mirror connection carries migrated
//! `mb_recording` and none of the app relations. A cross-database join —
//! the pre-fix shape — cannot plan on either side, and the seeds below
//! must satisfy the real schema, not a hand-written TEMP stand-in.
//! CI's disposable PostgreSQL owns the row data; no live mirror is required.
use nntune_api::handlers::{Store, StoreError};
use nntune_api::worker_entry::catalogue_store_for_postgres_test;
use std::cell::Cell;
use std::rc::Rc;
use tokio_postgres::{Client, Config, NoTls};

const USER: &str = "u1";
const LOCAL_TRACK: &str = "22222222-2222-2222-2222-222222222222";
const SHORT_RECORDING: &str = "33333333-3333-3333-3333-333333333333";
const LONG_RECORDING: &str = "44444444-4444-4444-4444-444444444444";
const LONG_TRACK: &str = "55555555-5555-5555-5555-555555555555";
const ORPHAN_RECORDING: &str = "77777777-7777-7777-7777-777777777777";
const UNKNOWN: &str = "66666666-6666-6666-6666-666666666666";
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
        !has_table(&app, "mb_recording").await,
        "app store must not name mb_recording"
    );
    let mirror = mirror().await;
    assert!(
        has_table(&mirror, "mb_recording").await,
        "mirror store misses mb_recording"
    );
    for table in ["tracks", "track_media", "cache_objects"] {
        assert!(
            !has_table(&mirror, table).await,
            "mirror store must not name {table}"
        );
    }
}

async fn seed_user_and_job(app: &Client) {
    app.batch_execute(&format!(
        "INSERT INTO users (id, email, created_at) VALUES ('{USER}', 'u1@ci.invalid', 1700000000) \
         ON CONFLICT (id) DO NOTHING; \
         INSERT INTO ingest_jobs (id, user_id, idempotency_key, status) \
         VALUES ('job-meter-1', '{USER}', 'meter-1', 'done') \
         ON CONFLICT (user_id, idempotency_key) DO NOTHING"
    ))
    .await
    .unwrap();
}

async fn seed_track(app: &Client, track: &str, recording: &str, duration: Option<i32>) {
    seed_user_and_job(app).await;
    let duration_sql = duration
        .map(|d| d.to_string())
        .unwrap_or_else(|| "NULL".into());
    app.batch_execute(&format!(
        "INSERT INTO tracks (id, mbid, release_id, title, medium_no, track_no, duration_ms) \
         VALUES ('{track}', '{recording}', '88888888-8888-8888-8888-888888888888', 'Track', 1, 1, {duration_sql}) \
         ON CONFLICT (id) DO NOTHING"
    ))
    .await
    .unwrap();
}

async fn seed_media(app: &Client, track: &str) {
    seed_user_and_job(app).await;
    let hash = "a".repeat(64);
    app.batch_execute(&format!(
        "INSERT INTO cache_objects (content_hash, byte_size, r2_bucket, r2_key) \
         VALUES ('{hash}', 2000000, 'ci-bucket', 'ci/key') \
         ON CONFLICT (content_hash) DO NOTHING; \
         INSERT INTO track_media (user_id, track_id, source_job_id, content_hash, format, bitrate, verified_at) \
         VALUES ('{USER}', '{track}', 'job-meter-1', '{hash}', 'flac', 128, 1700000200) \
         ON CONFLICT (user_id, track_id, source_job_id, content_hash) DO NOTHING"
    ))
    .await
    .unwrap();
}

async fn seed_recording(mirror: &Client, recording: &str, duration: i32) {
    mirror
        .batch_execute(&format!(
            "INSERT INTO mb_recording (mbid, title, duration_ms) \
             VALUES ('{recording}', 'Rec', {duration}) \
             ON CONFLICT (mbid) DO NOTHING"
        ))
        .await
        .unwrap();
}

fn meter_store(app: Client, mirror: Result<Client, StoreError>) -> (impl Store, Rc<Cell<usize>>) {
    let attempts = Rc::new(Cell::new(0));
    let store = catalogue_store_for_postgres_test(app, true, mirror, attempts.clone());
    (store, attempts)
}

#[tokio::test]
async fn local_uuid_resolves_app_and_mirror_rungs() {
    let app = app().await;
    seed_track(&app, LOCAL_TRACK, SHORT_RECORDING, Some(60_000)).await;
    seed_media(&app, LOCAL_TRACK).await;
    let mirror = mirror().await;
    seed_recording(&mirror, SHORT_RECORDING, 61_000).await;
    let (store, attempts) = meter_store(app, Ok(mirror));
    let meta = store.track_meter_meta(USER, LOCAL_TRACK).await.unwrap();
    assert_eq!(attempts.get(), 1, "configured mirror is probed once");
    assert_eq!(meta.track_duration_ms, Some(60_000));
    assert_eq!(meta.mb_duration_ms, Some(61_000));
    assert_eq!(meta.bitrate_kbps, Some(128));
    assert_eq!(meta.file_bytes, Some(2_000_000));
}

#[tokio::test]
async fn recording_mbid_resolves_artifact_and_short_mirror() {
    let app = app().await;
    seed_track(&app, LOCAL_TRACK, SHORT_RECORDING, Some(60_000)).await;
    let mirror = mirror().await;
    seed_recording(&mirror, SHORT_RECORDING, 61_000).await;
    let (store, attempts) = meter_store(app, Ok(mirror));
    let meta = store.track_meter_meta(USER, SHORT_RECORDING).await.unwrap();
    assert_eq!(attempts.get(), 1);
    assert_eq!(meta.track_duration_ms, Some(60_000));
    assert_eq!(meta.mb_duration_ms, Some(61_000));
    let duration = nntune_api::ledger::resolve_duration_ms(
        meta.track_duration_ms,
        meta.mb_duration_ms,
        meta.bitrate_kbps,
        meta.file_bytes,
    );
    // Known short duration wins over the 900 s fallback.
    assert_eq!(duration, Some(60_000));
    assert_eq!(
        nntune_api::playback::continuation_until(NOW, duration),
        NOW + 60 + nntune_api::playback::LEASE_TTL_SECS
    );
}

#[tokio::test]
async fn recording_mbid_without_artifact_uses_mirror_only() {
    let app = app().await;
    let mirror = mirror().await;
    seed_recording(&mirror, ORPHAN_RECORDING, 61_000).await;
    let (store, attempts) = meter_store(app, Ok(mirror));
    let meta = store
        .track_meter_meta(USER, ORPHAN_RECORDING)
        .await
        .unwrap();
    assert_eq!(attempts.get(), 1);
    assert_eq!(meta.track_duration_ms, None);
    assert_eq!(meta.mb_duration_ms, Some(61_000));
    assert_eq!(meta.bitrate_kbps, None);
    assert_eq!(meta.file_bytes, None);
    let duration = nntune_api::ledger::resolve_duration_ms(
        meta.track_duration_ms,
        meta.mb_duration_ms,
        meta.bitrate_kbps,
        meta.file_bytes,
    );
    assert_eq!(duration, Some(61_000));
}

#[tokio::test]
async fn long_recording_never_falls_back_to_900() {
    let app = app().await;
    seed_track(&app, LONG_TRACK, LONG_RECORDING, None).await;
    let mirror = mirror().await;
    seed_recording(&mirror, LONG_RECORDING, 1_500_000).await;
    let (store, attempts) = meter_store(app, Ok(mirror));
    let meta = store.track_meter_meta(USER, LONG_RECORDING).await.unwrap();
    assert_eq!(attempts.get(), 1);
    assert_eq!(meta.track_duration_ms, None);
    assert_eq!(meta.mb_duration_ms, Some(1_500_000));
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
async fn unknown_id_falls_back_without_error() {
    let app = app().await;
    let mirror = mirror().await;
    let (store, attempts) = meter_store(app, Ok(mirror));
    let meta = store.track_meter_meta(USER, UNKNOWN).await.unwrap();
    assert_eq!(attempts.get(), 1);
    assert_eq!(meta, nntune_api::meter_sink::TrackMeterMeta::default());
    let duration = nntune_api::ledger::resolve_duration_ms(
        meta.track_duration_ms,
        meta.mb_duration_ms,
        meta.bitrate_kbps,
        meta.file_bytes,
    );
    assert_eq!(duration, None);
    assert_eq!(
        nntune_api::playback::continuation_until(NOW, duration),
        NOW + 900
    );
}

#[tokio::test]
async fn unconfigured_mirror_degrades_without_attempt() {
    let app = app().await;
    seed_track(&app, LOCAL_TRACK, SHORT_RECORDING, Some(60_000)).await;
    let attempts = Rc::new(Cell::new(0));
    let store = catalogue_store_for_postgres_test(app, false, Err(StoreError), attempts.clone());
    let meta = store.track_meter_meta(USER, LOCAL_TRACK).await.unwrap();
    assert_eq!(attempts.get(), 0, "absence is not a failed connection");
    assert_eq!(meta.track_duration_ms, Some(60_000));
    assert_eq!(meta.mb_duration_ms, None);
}

#[tokio::test]
async fn configured_mirror_failure_never_degrades() {
    let app = app().await;
    seed_track(&app, LOCAL_TRACK, SHORT_RECORDING, Some(60_000)).await;
    let attempts = Rc::new(Cell::new(0));
    let store = catalogue_store_for_postgres_test(app, true, Err(StoreError), attempts.clone());
    assert!(store.track_meter_meta(USER, LOCAL_TRACK).await.is_err());
    assert_eq!(attempts.get(), 1);
}

#[tokio::test]
async fn non_uuid_needs_no_database() {
    // A non-UUID id short-circuits before any connection: the fresh
    // app client must stay untouched.
    let (store, attempts) = meter_store(app().await, Err(StoreError));
    let meta = store.track_meter_meta(USER, "track-9").await.unwrap();
    assert_eq!(meta, nntune_api::meter_sink::TrackMeterMeta::default());
    assert_eq!(attempts.get(), 0);
}
