//! Executes the production transaction, not copied SQL, on a disposable local
//! PostgreSQL service. Enabled explicitly with the postgres-tests feature.

use super::pool_registry::*;
use super::*;
use crate::pool_providers::{
    encrypt_pool_secret, PoolProviderRow, PoolProviderSecret, PoolProviderSettings,
    PoolProviderWrite, PoolProviderWriteOutcome, PoolSecretWrite,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio_postgres::{Client, NoTls};

static SCHEMA: AtomicUsize = AtomicUsize::new(0);
const ID: &str = "11111111-1111-4111-8111-111111111111";
const NOW: u64 = 1_800_000_000;

struct Fixture {
    config: tokio_postgres::Config,
    schema: String,
    db: Client,
}

impl Fixture {
    async fn new() -> Self {
        let url = std::env::var("NNTUNE_TEST_DATABASE_URL").expect("disposable PostgreSQL URL");
        let config: tokio_postgres::Config = url.parse().unwrap();
        assert_eq!(config.get_dbname(), Some("nntune_ci"));
        assert!(!config.get_hosts().is_empty());
        assert!(config.get_hosts().iter().all(|host| match host {
            tokio_postgres::config::Host::Tcp(host) =>
                host == "localhost"
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback()),
            _ => false,
        }));
        assert!(config.get_hostaddrs().iter().all(|ip| ip.is_loopback()));
        let schema = format!(
            "pool_test_{}_{}",
            std::process::id(),
            SCHEMA.fetch_add(1, Ordering::Relaxed)
        );
        let db = connect(&config, "").await;
        db.batch_execute(&format!(
            "CREATE SCHEMA {schema}; SET search_path={schema},pg_catalog; \
            CREATE TABLE users (id TEXT PRIMARY KEY, role TEXT NOT NULL, deleting_at BIGINT); \
            INSERT INTO users VALUES ('admin','admin',NULL),('member','member',NULL); \
            CREATE TABLE audit_log (user_id TEXT, action TEXT, ok BOOLEAN, source_ip TEXT, \
            at BIGINT, subject_user_id TEXT, details JSONB);"
        ))
        .await
        .unwrap();
        // Use the actual migration's table definitions and constraints, with
        // only the schema rebound to this test's isolated namespace.
        let ddl = include_str!("../../../../migrations/0035_pool_providers.sql");
        let ddl = ddl
            .split("CREATE TABLE IF NOT EXISTS public.pool_provider_secrets")
            .nth(1)
            .unwrap();
        let ddl = ddl.split("-- One-time attribution move").next().unwrap();
        let ddl = format!("CREATE TABLE IF NOT EXISTS public.pool_provider_secrets{ddl}")
            .replace("public.", &format!("{schema}."));
        db.batch_execute(&ddl).await.unwrap();
        Self { config, schema, db }
    }

    async fn connection(&self) -> Client {
        connect(&self.config, &self.schema).await
    }

    async fn cleanup(self) {
        self.db
            .batch_execute(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .await
            .unwrap();
    }

    async fn counts(&self) -> (i64, i64, i64) {
        let row = self
            .db
            .query_one(
                "SELECT (SELECT count(*) FROM pool_providers), \
            (SELECT count(*) FROM pool_provider_secrets), (SELECT count(*) FROM audit_log)",
                &[],
            )
            .await
            .unwrap();
        (row.get(0), row.get(1), row.get(2))
    }
}

async fn connect(config: &tokio_postgres::Config, schema: &str) -> Client {
    let (db, connection) = config.connect(NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.expect("scratch connection driver") });
    if !schema.is_empty() {
        db.batch_execute(&format!("SET search_path={schema},pg_catalog"))
            .await
            .unwrap();
    }
    db
}

fn write(revision: u64, rotate: bool) -> PoolProviderWrite {
    let secret = rotate.then(|| {
        let secret_ref = crate::providers::mint_provider_id().unwrap();
        let envelope = encrypt_pool_secret(
            &[7; 32],
            1,
            &secret_ref,
            &PoolProviderSecret {
                password: "synthetic-pool-password".into(),
            },
        )
        .unwrap();
        PoolSecretWrite {
            secret_ref,
            kek_version: 1,
            envelope,
        }
    });
    PoolProviderWrite {
        settings: PoolProviderSettings {
            id: ID.into(),
            label: "Synthetic account".into(),
            host: "news.example.test".into(),
            port: 563,
            tls: true,
            username: "synthetic-pool-username".into(),
            can_read: true,
            can_post: false,
            max_connections: 4,
            stream_reserve: 1,
            ingest_limit: 3,
            backbone: "synthetic".into(),
            tier: 0,
            enabled: true,
        },
        expected_revision: revision,
        secret,
    }
}

fn applied(outcome: PoolProviderWriteOutcome) -> PoolProviderRow {
    match outcome {
        PoolProviderWriteOutcome::Applied(row) => row,
        _ => panic!("write was not applied"),
    }
}

#[tokio::test]
async fn production_registry_write_rotation_and_redaction() {
    let f = Fixture::new().await;
    let mut db = f.connection().await;
    let first = applied(
        write_on(&mut db, "admin", &write(0, true), "", NOW)
            .await
            .unwrap(),
    );
    assert_eq!(first.revision, 1);
    assert_eq!(f.counts().await, (1, 1, 1));
    let mut update = write(1, false);
    update.settings.enabled = false;
    update.settings.max_connections = 3;
    update.settings.ingest_limit = 2;
    let second = applied(
        write_on(&mut db, "admin", &update, "", NOW + 1)
            .await
            .unwrap(),
    );
    assert_eq!(first.secret_ref, second.secret_ref);
    let rotated = applied(
        write_on(&mut db, "admin", &write(2, true), "", NOW + 2)
            .await
            .unwrap(),
    );
    assert_ne!(first.secret_ref, rotated.secret_ref);
    assert_eq!(rotated.revision, 3);
    assert_eq!(f.counts().await, (1, 1, 7));
    assert!(matches!(
        write_on(&mut db, "admin", &write(1, true), "", NOW + 3)
            .await
            .unwrap(),
        PoolProviderWriteOutcome::Conflict
    ));
    assert_eq!(f.counts().await, (1, 1, 7));
    let rows = list_on(&db).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].secret_ref, rotated.secret_ref);
    assert_eq!(rows[0].settings.username, "synthetic-pool-username");
    let audit =
        f.db.query("SELECT action, details::text FROM audit_log", &[])
            .await
            .unwrap();
    let actions: Vec<String> = audit.iter().map(|r| r.get(0)).collect();
    assert!(actions.iter().any(|a| a == "pool_provider.disabled"));
    assert!(actions.iter().any(|a| a == "pool_provider.caps_changed"));
    assert!(actions
        .iter()
        .any(|a| a == "pool_provider.credentials_rotated"));
    for row in audit {
        let details: String = row.get(1);
        assert!(!details.contains("synthetic-pool-password"));
        assert!(!details.contains("synthetic-pool-username"));
        assert!(!details.contains(&first.secret_ref));
        assert!(!details.contains(&rotated.secret_ref));
    }
    f.cleanup().await;
}

#[tokio::test]
async fn production_registry_denies_demoted_and_deleting_actors() {
    let f = Fixture::new().await;
    let mut db = f.connection().await;
    for actor in ["member", "absent"] {
        assert!(matches!(
            write_on(&mut db, actor, &write(0, true), "", NOW)
                .await
                .unwrap(),
            PoolProviderWriteOutcome::ActorNotAdmin
        ));
    }
    f.db.batch_execute("UPDATE users SET deleting_at=1 WHERE id='admin'")
        .await
        .unwrap();
    assert!(matches!(
        write_on(&mut db, "admin", &write(0, true), "", NOW)
            .await
            .unwrap(),
        PoolProviderWriteOutcome::ActorNotAdmin
    ));
    assert_eq!(f.counts().await, (0, 0, 0));
    f.cleanup().await;
}

#[tokio::test]
async fn production_registry_audit_failure_rolls_back_everything() {
    let f = Fixture::new().await;
    let mut db = f.connection().await;
    f.db.batch_execute("ALTER TABLE audit_log ADD CHECK (NOT ok)")
        .await
        .unwrap();
    assert!(write_on(&mut db, "admin", &write(0, true), "", NOW)
        .await
        .is_err());
    assert_eq!(f.counts().await, (0, 0, 0));
    f.db.batch_execute("ALTER TABLE audit_log DROP CONSTRAINT audit_log_ok_check")
        .await
        .unwrap();
    let first = applied(
        write_on(&mut db, "admin", &write(0, true), "", NOW)
            .await
            .unwrap(),
    );
    f.db.batch_execute("ALTER TABLE audit_log ADD CHECK (action='pool_provider.created')")
        .await
        .unwrap();
    assert!(write_on(&mut db, "admin", &write(1, true), "", NOW + 1)
        .await
        .is_err());
    assert_eq!(f.counts().await, (1, 1, 1));
    let rows = list_on(&db).await.unwrap();
    assert_eq!(rows[0].secret_ref, first.secret_ref);
    assert_eq!(rows[0].revision, 1);
    f.cleanup().await;
}

#[tokio::test]
async fn production_registry_concurrent_cas_has_one_winner() {
    let f = Fixture::new().await;
    let mut a = f.connection().await;
    let mut b = f.connection().await;
    applied(
        write_on(&mut a, "admin", &write(0, true), "", NOW)
            .await
            .unwrap(),
    );
    let wa = write(1, true);
    let wb = write(1, true);
    let (a, b) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            write_on(&mut a, "admin", &wa, "", NOW + 1),
            write_on(&mut b, "admin", &wb, "", NOW + 1)
        )
    })
    .await
    .expect("concurrent writes finish");
    assert!(matches!(
        (&a, &b),
        (
            Ok(PoolProviderWriteOutcome::Applied(_)),
            Ok(PoolProviderWriteOutcome::Conflict)
        ) | (
            Ok(PoolProviderWriteOutcome::Conflict),
            Ok(PoolProviderWriteOutcome::Applied(_))
        )
    ));
    assert_eq!(f.counts().await, (1, 1, 3));
    assert_eq!(list_on(&f.db).await.unwrap()[0].revision, 2);
    f.cleanup().await;
}
