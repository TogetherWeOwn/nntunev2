//! Real production transaction/bindings, on CI's disposable PostgreSQL service.
//! Run with --features postgres-tests --test session_refresh_postgres. Missing
//! test configuration fails; ordinary unit tests do not require a database.
use nntune_api::session::SessionClaims;
use nntune_api::session_retry_store::rotate_with_retry;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio_postgres::{Client, Config, NoTls};

static SCHEMA: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    config: Config,
    schema: String,
    db: Client,
    old: SessionClaims,
    next: SessionClaims,
}

async fn connect(config: &Config, schema: &str) -> Client {
    let (client, connection) = config
        .connect(NoTls)
        .await
        .expect("connect scratch PostgreSQL");
    tokio::spawn(async move { connection.await.expect("scratch connection driver") });
    if !schema.is_empty() {
        client
            .batch_execute(&format!("SET search_path TO {schema}"))
            .await
            .unwrap();
    }
    client
}

async fn clock(db: &Client) -> i64 {
    db.query_one(
        "SELECT floor(extract(epoch FROM clock_timestamp()))::bigint",
        &[],
    )
    .await
    .unwrap()
    .get(0)
}

impl Fixture {
    async fn new() -> Self {
        let url = std::env::var("NNTUNE_TEST_DATABASE_URL")
            .expect("NNTUNE_TEST_DATABASE_URL must name CI's disposable database");
        let config: Config = url.parse().expect("scratch database configuration");
        // No arbitrary database URLs: this regression is wired only to a local
        // throwaway service, never an application or production credential.
        assert_eq!(config.get_dbname(), Some("nntune_ci"));
        assert!(!config.get_hosts().is_empty());
        assert!(config.get_hosts().iter().all(|host| match host {
            tokio_postgres::config::Host::Tcp(host) => {
                host == "localhost"
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            }
            _ => false,
        }));
        let schema = format!(
            "retry_test_{}_{}",
            std::process::id(),
            SCHEMA.fetch_add(1, Ordering::Relaxed)
        );
        let db = connect(&config, "").await;
        db.batch_execute(&format!(
            "CREATE SCHEMA {schema}; SET search_path TO {schema}"
        ))
        .await
        .unwrap();
        db.batch_execute(include_str!("../../../migrations/0002_auth_vault.sql"))
            .await
            .unwrap();
        db.batch_execute(include_str!(
            "../../../migrations/0024_session_refresh_retry.sql"
        ))
        .await
        .unwrap();
        let now = clock(&db).await;
        db.execute(
            "INSERT INTO users (id,email,created_at) VALUES ('u1','synthetic@example.test',$1)",
            &[&now],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO sessions (id,user_id,created_at,expires_at) VALUES ('s1','u1',$1,$2)",
            &[&now, &(now + 3600)],
        )
        .await
        .unwrap();
        let old = SessionClaims {
            session_id: "s1".into(),
            user_id: "u1".into(),
            created_at: now as u64,
            issued_at: now as u64,
            expires_at: (now + 3600) as u64,
            rotation_counter: 0,
        };
        let next = SessionClaims {
            rotation_counter: 1,
            ..old.clone()
        };
        Self {
            config,
            schema,
            db,
            old,
            next,
        }
    }

    async fn client(&self) -> Client {
        connect(&self.config, &self.schema).await
    }

    async fn wait_for_lock(&self, pid: i32) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let waiting: bool = self
                    .db
                    .query_one("SELECT cardinality(pg_blocking_pids($1)) > 0", &[&pid])
                    .await
                    .unwrap()
                    .get(0);
                if waiting {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("contender must actually wait on another PostgreSQL connection");
    }

    async fn wait_until(&self, deadline: i64) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while clock(&self.db).await < deadline {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cross the claim deadline while the row remains locked");
    }
}

#[tokio::test]
async fn real_adapter_same_key_contenders_converge_and_different_keys_lose() {
    let f = Fixture::new().await;
    let mut locker = f.client().await;
    let lock = locker.transaction().await.unwrap();
    lock.query_one("SELECT id FROM sessions WHERE id='s1' FOR UPDATE", &[])
        .await
        .unwrap();
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let mut client = f.client().await;
        let pid = client
            .query_one("SELECT pg_backend_pid()", &[])
            .await
            .unwrap()
            .get(0);
        let old = f.old.clone();
        let next = f.next.clone();
        tasks.push(tokio::spawn(async move {
            rotate_with_retry(&mut client, &old, &next, "same-client-digest")
                .await
                .unwrap()
                .unwrap()
        }));
        f.wait_for_lock(pid).await;
    }
    lock.commit().await.unwrap();
    let first = tasks.remove(0).await.unwrap();
    for task in tasks {
        assert_eq!(task.await.unwrap(), first);
    }
    assert_eq!(first.rotation_counter, 1);
    assert_eq!(first.expires_at, f.next.expires_at);
    let lock = locker.transaction().await.unwrap();
    lock.query_one("SELECT id FROM sessions WHERE id='s1' FOR UPDATE", &[])
        .await
        .unwrap();
    let mut tasks = Vec::new();
    for digest in [
        "same-client-digest",
        "different-client-digest",
        "same-client-digest",
        "other-digest",
    ] {
        let mut client = f.client().await;
        let pid = client
            .query_one("SELECT pg_backend_pid()", &[])
            .await
            .unwrap()
            .get(0);
        let old = f.old.clone();
        let next = f.next.clone();
        tasks.push((
            digest == "same-client-digest",
            tokio::spawn(async move {
                rotate_with_retry(&mut client, &old, &next, digest)
                    .await
                    .unwrap()
            }),
        ));
        f.wait_for_lock(pid).await;
    }
    lock.commit().await.unwrap();
    for (same_key, task) in tasks {
        assert_eq!(task.await.unwrap(), same_key.then(|| first.clone()));
    }
}

#[tokio::test]
async fn real_adapter_uses_claim_order_not_request_timestamp_order() {
    let f = Fixture::new().await;
    let mut client = f.client().await;
    // The earlier-sampled request is already prepared but delayed before DB
    // admission. The later-sampled request commits first across a clock tick.
    let earlier = f.next.clone();
    f.wait_until(earlier.issued_at as i64 + 1).await;
    let later = SessionClaims {
        issued_at: earlier.issued_at + 1,
        ..earlier.clone()
    };
    let first = rotate_with_retry(&mut client, &f.old, &later, "bound-digest")
        .await
        .unwrap()
        .unwrap();
    let recovered = rotate_with_retry(&mut client, &f.old, &earlier, "bound-digest")
        .await
        .unwrap();
    assert_eq!(recovered, Some(first.clone()));
    assert!(first.issued_at > earlier.issued_at);
}

#[tokio::test]
async fn real_adapter_rejects_receipt_that_expires_during_unchanged_row_lock_wait() {
    let f = Fixture::new().await;
    let mut client = f.client().await;
    rotate_with_retry(&mut client, &f.old, &f.next, "bound-digest")
        .await
        .unwrap()
        .unwrap();
    let issued_at = clock(&f.db).await - 119;
    f.db.execute("UPDATE sessions SET refresh_issued_at=$1", &[&issued_at])
        .await
        .unwrap();
    let mut locker = f.client().await;
    let lock = locker.transaction().await.unwrap();
    // The holder makes NO change: a volatile predicate in a waiting UPDATE
    // must not be mistaken for a clock sampled after serialization.
    lock.query_one("SELECT id FROM sessions WHERE id='s1' FOR UPDATE", &[])
        .await
        .unwrap();
    let pid = client
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    let old = f.old.clone();
    let next = f.next.clone();
    let task = tokio::spawn(async move {
        rotate_with_retry(&mut client, &old, &next, "bound-digest")
            .await
            .unwrap()
    });
    f.wait_for_lock(pid).await;
    f.wait_until(issued_at + 120).await;
    lock.commit().await.unwrap();
    assert!(task.await.unwrap().is_none());
    let receipt: i64 =
        f.db.query_one("SELECT refresh_issued_at FROM sessions", &[])
            .await
            .unwrap()
            .get(0);
    assert_eq!(receipt, issued_at, "rejection cannot renew the receipt");
}

#[tokio::test]
async fn real_adapter_rechecks_predecessor_and_successor_expiry_after_lock_wait() {
    for predecessor_expires in [true, false] {
        let f = Fixture::new().await;
        let mut client = f.client().await;
        let expires = clock(&f.db).await + 2;
        let mut old = f.old.clone();
        let mut next = f.next.clone();
        if predecessor_expires {
            old.expires_at = expires as u64;
        } else {
            next.expires_at = expires as u64;
        }
        f.db.execute(
            "UPDATE sessions SET expires_at=$1",
            &[&(old.expires_at as i64)],
        )
        .await
        .unwrap();
        rotate_with_retry(&mut client, &old, &next, "bound-digest")
            .await
            .unwrap()
            .unwrap();
        let mut locker = f.client().await;
        let lock = locker.transaction().await.unwrap();
        lock.query_one("SELECT id FROM sessions WHERE id='s1' FOR UPDATE", &[])
            .await
            .unwrap();
        let pid = client
            .query_one("SELECT pg_backend_pid()", &[])
            .await
            .unwrap()
            .get(0);
        let task = tokio::spawn(async move {
            rotate_with_retry(&mut client, &old, &next, "bound-digest")
                .await
                .unwrap()
        });
        f.wait_for_lock(pid).await;
        f.wait_until(expires).await;
        lock.commit().await.unwrap();
        assert!(task.await.unwrap().is_none());
    }
}
