//! Native-only tests of the production Transaction implementation. CI runs
//! these explicitly against its disposable PostgreSQL service; ordinary unit
//! tests do not require a database or silently substitute SQLite.

use super::*;
use futures_util::future::{select, Either};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;
use tokio_postgres::{config::Host, Client, Config, NoTls};

fn runtime() -> Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn disposable_config(database_url: &str) -> Result<Config, &'static str> {
    let config: Config = database_url
        .parse()
        .map_err(|_| "invalid test database URL")?;
    // Validate the driver's effective settings, including URL query overrides,
    // and connect with this same configuration before issuing any fixture DDL.
    if !matches!(config.get_dbname(), Some("nntune_ci" | "agent-testdb")) {
        return Err("test database must be disposable");
    }
    if config.get_hosts().is_empty()
        || !config.get_hosts().iter().all(|host| match host {
            Host::Tcp(host) => matches!(host.as_str(), "localhost" | "127.0.0.1" | "agent-testdb"),
            _ => false,
        })
        || !config.get_hostaddrs().iter().all(|addr| addr.is_loopback())
    {
        return Err("test database hosts and addresses must be local");
    }
    Ok(config)
}

#[test]
fn disposable_config_checks_effective_driver_settings() {
    let base = "postgresql://postgres:throwaway@localhost:5432/nntune_ci";
    for url in [
        base.to_owned(),
        "postgresql://postgres@agent-testdb/agent-testdb".to_owned(),
        format!("{base}?hostaddr=127.0.0.1"),
        format!("{base}?dbname=agent-testdb"),
    ] {
        assert!(
            disposable_config(&url).is_ok(),
            "allowed disposable configuration"
        );
    }
    for url in [
        format!("{base}?dbname=application"),
        format!("{base}?dbname=nntune_ci&dbname=application"),
        format!("{base}?host=remote.example.com"),
        format!("{base}?host=localhost,remote.example.com"),
        format!("{base}?host=%2Fvar%2Frun%2Fpostgresql"),
        format!("{base}?hostaddr=203.0.113.10"),
        format!("{base}?hostaddr=127.0.0.1,203.0.113.10"),
        "postgresql://postgres@remote.example.com/nntune_ci".to_owned(),
        "postgresql://postgres@localhost/application".to_owned(),
    ] {
        assert!(
            disposable_config(&url).is_err(),
            "unsafe configuration must fail before connecting"
        );
    }
    let config = disposable_config(&format!("{base}?dbname=agent-testdb&hostaddr=127.0.0.1"))
        .expect("checked effective configuration");
    assert_eq!(config.get_dbname(), Some("agent-testdb"));
    assert_eq!(
        config.get_hostaddrs(),
        &[std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)]
    );
}

async fn connect() -> (Client, JoinHandle<()>) {
    let database_url = std::env::var("NNTUNE_REDEMPTION_TEST_DATABASE_URL")
        .expect("set the dedicated disposable redemption-test database URL");
    let config = disposable_config(&database_url).expect("unsafe test database configuration");
    let (client, connection) = config
        .connect(NoTls)
        .await
        .expect("connect to disposable PostgreSQL");
    let driver = tokio::spawn(async move {
        connection.await.expect("PostgreSQL connection driver");
    });
    // Ensure the transaction implementation, not a connection default,
    // supplies the bound tested below.
    client
        .batch_execute("SET statement_timeout = 0")
        .await
        .unwrap();
    (client, driver)
}

struct Fixture {
    client: Client,
    driver: JoinHandle<()>,
    schema: String,
    magic: MagicLinkRecord,
    session: SessionRecord,
}

impl Fixture {
    async fn new() -> Self {
        static NEXT_SCHEMA: AtomicU64 = AtomicU64::new(0);
        let (client, driver) = connect().await;
        let schema = format!(
            "redemption_{}_{}",
            std::process::id(),
            NEXT_SCHEMA.fetch_add(1, Ordering::Relaxed)
        );
        client
            .batch_execute(&format!(
                "CREATE SCHEMA {schema}; SET search_path TO {schema};
                 CREATE TABLE users (id TEXT PRIMARY KEY);
                 CREATE TABLE magic_link_tokens (
                    token_hash TEXT PRIMARY KEY, user_id TEXT REFERENCES users(id),
                    email TEXT, issued_at BIGINT, expires_at BIGINT, used BOOLEAN);
                 CREATE TABLE sessions (
                    id TEXT PRIMARY KEY, user_id TEXT REFERENCES users(id),
                    created_at BIGINT, expires_at BIGINT, rotation_counter INTEGER,
                    revoked BOOLEAN);
                 CREATE TABLE audit_log (
                    user_id TEXT, action TEXT, ok BOOLEAN, source_ip TEXT, at BIGINT);
                 INSERT INTO users VALUES ('u1');"
            ))
            .await
            .unwrap();
        let now: i64 = client
            .query_one(
                "SELECT FLOOR(EXTRACT(EPOCH FROM clock_timestamp()))::BIGINT",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        client
            .execute(
                "INSERT INTO magic_link_tokens VALUES ('hash', 'u1', 'a@example.com', $1, $2, FALSE)",
                &[&now, &(now + 900)],
            )
            .await
            .unwrap();
        let now = u64::try_from(now).unwrap();
        Self {
            client,
            driver,
            schema,
            magic: MagicLinkRecord {
                token_hash: "hash".into(),
                user_id: "u1".into(),
                email: "a@example.com".into(),
                issued_at: now,
                expires_at: now + 900,
                used: true,
            },
            session: SessionRecord {
                id: "session".into(),
                user_id: "u1".into(),
                created_at: now,
                expires_at: now + 3600,
                rotation_counter: 0,
                revoked: false,
            },
        }
    }

    fn audit(&self) -> AuditEvent {
        crate::vault::audit_event(
            "u1",
            "auth.magic",
            true,
            "203.0.113.9",
            self.magic.issued_at,
        )
    }

    async fn persisted(&self) -> (bool, i64, i64) {
        let row = self
            .client
            .query_one(
                "SELECT used, (SELECT COUNT(*) FROM sessions),
                 (SELECT COUNT(*) FROM audit_log) FROM magic_link_tokens",
                &[],
            )
            .await
            .unwrap();
        (row.get(0), row.get(1), row.get(2))
    }

    async fn close(self) {
        self.client
            .batch_execute(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .await
            .unwrap();
        drop(self.client);
        self.driver.await.unwrap();
    }
}

#[test]
#[ignore = "requires the disposable PostgreSQL service; enforced explicitly in CI"]
fn postgres_claim_uses_database_time() {
    runtime().block_on(async {
        let mut fixture = Fixture::new().await;
        fixture.magic.expires_at = fixture.magic.issued_at - 1;
        fixture
            .client
            .execute(
                "UPDATE magic_link_tokens SET expires_at = $1",
                &[&i64::try_from(fixture.magic.expires_at).unwrap()],
            )
            .await
            .unwrap();
        let audit = fixture.audit();
        let transaction = fixture.client.transaction().await.unwrap();
        assert!(matches!(
            redeem(transaction, &fixture.magic, &fixture.session, Some(&audit)).await,
            Ok(false)
        ));
        assert_eq!(fixture.persisted().await, (false, 0, 0));
        fixture.close().await;
    });
}

#[test]
#[ignore = "requires the disposable PostgreSQL service; enforced explicitly in CI"]
fn postgres_claim_rejects_link_expiring_after_transaction_start() {
    runtime().block_on(async {
        let mut fixture = Fixture::new().await;
        let audit = fixture.audit();
        let transaction = fixture.client.transaction().await.unwrap();
        transaction.apply_statement_timeout().await.unwrap();
        let expires_at: i64 = transaction
            .query_one(
                "UPDATE magic_link_tokens
                 SET expires_at = FLOOR(EXTRACT(EPOCH FROM clock_timestamp()))::BIGINT + 2
                 RETURNING expires_at",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        fixture.magic.expires_at = u64::try_from(expires_at).unwrap();
        // Cross the expiry using database time while keeping this transaction
        // open. A frozen transaction_timestamp() claim would now accept it.
        tokio::time::timeout(
            Duration::from_secs(5),
            transaction.batch_execute("SELECT pg_sleep(2)"),
        )
        .await
        .expect("database-time expiry wait must be bounded")
        .unwrap();
        let clocks = transaction
            .query_one(
                "SELECT FLOOR(EXTRACT(EPOCH FROM transaction_timestamp()))::BIGINT < $1,
                        FLOOR(EXTRACT(EPOCH FROM clock_timestamp()))::BIGINT >= $1",
                &[&expires_at],
            )
            .await
            .unwrap();
        assert!(clocks.get::<_, bool>(0), "link valid at transaction start");
        assert!(clocks.get::<_, bool>(1), "link expired before claim");
        assert!(matches!(
            redeem(transaction, &fixture.magic, &fixture.session, Some(&audit)).await,
            Ok(false)
        ));
        assert_eq!(fixture.persisted().await, (false, 0, 0));
        fixture.close().await;
    });
}

#[test]
#[ignore = "requires the disposable PostgreSQL service; enforced explicitly in CI"]
fn postgres_claim_locks_user_before_token() {
    runtime().block_on(async {
        let mut fixture = Fixture::new().await;
        let (blocker, driver) = connect().await;
        blocker
            .batch_execute(&format!("SET search_path TO {}; BEGIN", fixture.schema))
            .await
            .unwrap();
        blocker
            .query_one("SELECT id FROM users WHERE id = 'u1' FOR UPDATE", &[])
            .await
            .unwrap();
        let pid: i32 = fixture
            .client
            .query_one("SELECT pg_backend_pid()", &[])
            .await
            .unwrap()
            .get(0);
        let transaction = fixture.client.transaction().await.unwrap();
        transaction.apply_statement_timeout().await.unwrap();
        {
            let claim = std::pin::pin!(transaction.claim(&fixture.magic));
            let wait_for_user_lock = std::pin::pin!(tokio::time::timeout(
                Duration::from_secs(5),
                async {
                    loop {
                        blocker
                            .batch_execute("SELECT pg_stat_clear_snapshot()")
                            .await
                            .unwrap();
                        let waiting: bool = blocker
                            .query_one(
                                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity
                                 WHERE pid = $1 AND wait_event_type = 'Lock')",
                                &[&pid],
                            )
                            .await
                            .unwrap()
                            .get(0);
                        if waiting {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            ));
            let claim = match select(claim, wait_for_user_lock).await {
                Either::Left((result, _)) => {
                    panic!("claim did not wait for the locked user: {result:?}")
                }
                Either::Right((waited, claim)) => {
                    waited.expect("claim must reach the user-row lock");
                    claim
                }
            };
            // The claim is waiting on the user. It must NOT already own the
            // token: the reaper needs precisely this user-then-token order.
            blocker
                .query_one(
                    "SELECT token_hash FROM magic_link_tokens WHERE token_hash = 'hash' FOR UPDATE NOWAIT",
                    &[],
                )
                .await
                .expect("token must remain unlocked while claim waits on user");
            blocker.batch_execute("ROLLBACK").await.unwrap();
            assert_eq!(claim.await.unwrap(), 1);
        }
        transaction.rollback().await.unwrap();
        assert_eq!(fixture.persisted().await, (false, 0, 0));
        drop(blocker);
        driver.await.unwrap();
        fixture.close().await;
    });
}

#[test]
#[ignore = "requires the disposable PostgreSQL service; enforced explicitly in CI"]
fn postgres_local_timeout_aborts_audit_and_rolls_back_both_writes() {
    runtime().block_on(async {
        let mut fixture = Fixture::new().await;
        let transaction = fixture.client.transaction().await.unwrap();
        transaction.apply_statement_timeout().await.unwrap();
        let effective_ms: i64 = transaction
            .query_one(
                "SELECT setting::BIGINT FROM pg_settings WHERE name = 'statement_timeout'",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(
            effective_ms,
            i64::try_from(crate::vault::TOTAL_TIMEOUT_MS).unwrap()
        );
        transaction.rollback().await.unwrap();
        let reset: String = fixture
            .client
            .query_one("SHOW statement_timeout", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(reset, "0", "SET LOCAL must not leak beyond its transaction");
        fixture
            .client
            .batch_execute(
                "CREATE FUNCTION stall_audit() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN
               IF NOT (SELECT used FROM magic_link_tokens WHERE token_hash = 'hash')
                  OR (SELECT COUNT(*) FROM sessions) <> 1 THEN
                 RAISE EXCEPTION 'audit must follow claim and session insertion';
               END IF;
               PERFORM pg_sleep(45);
               RETURN NEW;
             END $$;
             CREATE TRIGGER stall_audit BEFORE INSERT ON audit_log
             FOR EACH ROW EXECUTE FUNCTION stall_audit();",
            )
            .await
            .unwrap();
        let audit = fixture.audit();
        let transaction = fixture.client.transaction().await.unwrap();
        let started = Instant::now();
        let result = tokio::time::timeout(
            Duration::from_millis(crate::vault::TOTAL_TIMEOUT_MS + 10_000),
            redeem(transaction, &fixture.magic, &fixture.session, Some(&audit)),
        )
        .await
        .expect("server timeout must fire before the outer test deadline");
        assert!(result.is_err());
        assert!(started.elapsed() >= Duration::from_millis(crate::vault::TOTAL_TIMEOUT_MS - 1_000));
        assert_eq!(fixture.persisted().await, (false, 0, 0));

        fixture
            .client
            .batch_execute("DROP TRIGGER stall_audit ON audit_log")
            .await
            .unwrap();
        let transaction = fixture.client.transaction().await.unwrap();
        assert!(matches!(
            redeem(transaction, &fixture.magic, &fixture.session, Some(&audit)).await,
            Ok(true)
        ));
        assert_eq!(fixture.persisted().await, (true, 1, 1));
        let transaction = fixture.client.transaction().await.unwrap();
        assert!(matches!(
            redeem(transaction, &fixture.magic, &fixture.session, Some(&audit)).await,
            Ok(false)
        ));
        assert_eq!(fixture.persisted().await, (true, 1, 1));
        fixture.close().await;
    });
}
