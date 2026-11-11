//! CI schema-only harness for independently migrated app and mirror stores.
//! It verifies relation separation between the two migration histories.
//! This does not exercise the production adapter or its metadata behavior.
//! The stores are disposable and prepared by the dedicated CI step.
use tokio_postgres::{Client, Config, NoTls};

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
