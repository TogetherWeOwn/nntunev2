//! Production `track_meter_meta` with two separately-migrated schemas.
//! The app connection sees only app tables (no `mb_recording`), the
//! mirror connection only mirror tables, so a cross-database join —
//! the pre-fix shape — cannot plan on either side. CI's disposable
//! PostgreSQL owns the row data; no live mirror is required.
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

async fn connect() -> Client {
    let config: Config = std::env::var("NNTUNE_TEST_DATABASE_URL")
        .expect("CI disposable database is required; never silently skip")
        .parse()
        .unwrap();
    assert_eq!(config.get_dbname(), Some("nntune_ci"));
    assert!(config.get_hostaddrs().is_empty(), "no address overrides");
    assert!(
        matches!(config.get_hosts(), [tokio_postgres::config::Host::Tcp(host)]
        if matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1"))
    );
    let (client, connection) = config.connect(NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    client
}

/// App-side schema only: `mb_recording` does not exist here, exactly
/// like the separately-migrated app database.
async fn app() -> Client {
    let client = connect().await;
    client
        .batch_execute(
            "SET statement_timeout = '3s'; SET search_path = pg_temp;
             CREATE TEMP TABLE tracks (id UUID PRIMARY KEY, mbid UUID NULL, duration_ms INTEGER NULL);
             CREATE TEMP TABLE track_media (user_id TEXT, track_id UUID, content_hash TEXT,
               bitrate INTEGER, verified_at BIGINT);
             CREATE TEMP TABLE cache_objects (content_hash TEXT PRIMARY KEY, byte_size BIGINT)",
        )
        .await
        .unwrap();
    client
}

/// Mirror-side schema only: the app tables do not exist here.
async fn mirror() -> Client {
    let client = connect().await;
    client
        .batch_execute(
            "SET statement_timeout = '3s'; SET search_path = pg_temp;
             CREATE TEMP TABLE mb_recording (mbid UUID PRIMARY KEY, title TEXT, duration_ms INTEGER NULL)",
        )
        .await
        .unwrap();
    client
}

#[tokio::test]
async fn local_uuid_resolves_app_and_mirror_rungs() {
    let app = app().await;
    app.batch_execute(&format!(
        "INSERT INTO tracks VALUES ('{LOCAL_TRACK}', '{SHORT_RECORDING}', 60000)"
    ))
    .await
    .unwrap();
    app.batch_execute(&format!(
        "INSERT INTO track_media VALUES ('u1', '{LOCAL_TRACK}', repeat('a',64), 128, 1700000200)"
    ))
    .await
    .unwrap();
    app.execute(
        "INSERT INTO cache_objects VALUES (repeat('a',64), 2000000)",
        &[],
    )
    .await
    .unwrap();
    let mirror = mirror().await;
    mirror
        .batch_execute(&format!(
            "INSERT INTO mb_recording VALUES ('{SHORT_RECORDING}', 'Short', 61000)"
        ))
        .await
        .unwrap();
    let attempts = Rc::new(Cell::new(0));
    let store = catalogue_store_for_postgres_test(app, true, Ok(mirror), attempts.clone());
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
    app.batch_execute(&format!(
        "INSERT INTO tracks VALUES ('{LOCAL_TRACK}', '{SHORT_RECORDING}', 60000)"
    ))
    .await
    .unwrap();
    let mirror = mirror().await;
    mirror
        .batch_execute(&format!(
            "INSERT INTO mb_recording VALUES ('{SHORT_RECORDING}', 'Short', 61000)"
        ))
        .await
        .unwrap();
    let attempts = Rc::new(Cell::new(0));
    let store = catalogue_store_for_postgres_test(app, true, Ok(mirror), attempts.clone());
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
    mirror
        .batch_execute(&format!(
            "INSERT INTO mb_recording VALUES ('{ORPHAN_RECORDING}', 'Orphan', 61000)"
        ))
        .await
        .unwrap();
    let attempts = Rc::new(Cell::new(0));
    let store = catalogue_store_for_postgres_test(app, true, Ok(mirror), attempts.clone());
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
    app.batch_execute(&format!(
        "INSERT INTO tracks VALUES ('{LONG_TRACK}', '{LONG_RECORDING}', NULL)"
    ))
    .await
    .unwrap();
    let mirror = mirror().await;
    mirror
        .batch_execute(&format!(
            "INSERT INTO mb_recording VALUES ('{LONG_RECORDING}', 'Long', 1500000)"
        ))
        .await
        .unwrap();
    let attempts = Rc::new(Cell::new(0));
    let store = catalogue_store_for_postgres_test(app, true, Ok(mirror), attempts.clone());
    let meta = store.track_meter_meta(USER, LONG_RECORDING).await.unwrap();
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
    let attempts = Rc::new(Cell::new(0));
    let store = catalogue_store_for_postgres_test(app, true, Ok(mirror), attempts.clone());
    let meta = store.track_meter_meta(USER, UNKNOWN).await.unwrap();
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
    app.batch_execute(&format!(
        "INSERT INTO tracks VALUES ('{LOCAL_TRACK}', '{SHORT_RECORDING}', 60000)"
    ))
    .await
    .unwrap();
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
    app.batch_execute(&format!(
        "INSERT INTO tracks VALUES ('{LOCAL_TRACK}', '{SHORT_RECORDING}', 60000)"
    ))
    .await
    .unwrap();
    let attempts = Rc::new(Cell::new(0));
    let store = catalogue_store_for_postgres_test(app, true, Err(StoreError), attempts.clone());
    assert!(store.track_meter_meta(USER, LOCAL_TRACK).await.is_err());
    assert_eq!(attempts.get(), 1);
}

#[tokio::test]
async fn non_uuid_needs_no_database() {
    // A non-UUID id short-circuits before any connection: the fresh
    // app client must stay untouched.
    let attempts = Rc::new(Cell::new(0));
    let store =
        catalogue_store_for_postgres_test(app().await, true, Err(StoreError), attempts.clone());
    let meta = store.track_meter_meta(USER, "track-9").await.unwrap();
    assert_eq!(meta, nntune_api::meter_sink::TrackMeterMeta::default());
    assert_eq!(attempts.get(), 0);
}
