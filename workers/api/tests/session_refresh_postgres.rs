//! Real production transaction/bindings, on CI's disposable PostgreSQL service.
//! Run with --features postgres-tests --test session_refresh_postgres. Missing
//! test configuration fails; ordinary unit tests do not require a database.
use hmac::{Hmac, KeyInit, Mac};
use nntune_api::handlers::{self, AsyncDns, Dialer, Mailer, Request, Response, StoreError};
use nntune_api::session::{verify_session, SessionClaims, COOKIE_NAME};
use nntune_api::session_retry_store::rotate_with_retry;
use sha2::Sha256;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_postgres::{Client, Config, NoTls};

static SCHEMA: AtomicUsize = AtomicUsize::new(0);
const SIGNING_KEY: &[u8] = b"synthetic-test-key-0123456789abcdef";
const ATTEMPTS: usize = 3;

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

fn token(claims: &SessionClaims) -> String {
    let payload = nntune_api::b64::encode(&serde_json::to_vec(claims).unwrap());
    let mut mac = Hmac::<Sha256>::new_from_slice(SIGNING_KEY).unwrap();
    mac.update(payload.as_bytes());
    format!(
        "{}.{}",
        payload,
        nntune_api::b64::encode(&mac.finalize().into_bytes())
    )
}

async fn joined<T>(mut task: JoinHandle<T>) -> T {
    match tokio::time::timeout(Duration::from_secs(12), &mut task).await {
        Ok(result) => result.expect("contender task"),
        Err(_) => {
            task.abort();
            let _ = task.await;
            panic!("contender did not finish after lock release")
        }
    }
}

// Counter, expiry and the complete receipt, including nullable columns.
async fn row_state(db: &Client) -> (i32, i64, Option<String>, Option<i64>, Option<i64>) {
    let row = db.query_one(
        "SELECT rotation_counter, expires_at, refresh_retry_hash, refresh_issued_at, refresh_claimed_at FROM sessions WHERE id='s1'",
        &[],
    ).await.unwrap();
    (row.get(0), row.get(1), row.get(2), row.get(3), row.get(4))
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
        db.batch_execute(include_str!(
            "../../../migrations/0025_session_refresh_claim_clock.sql"
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

    async fn wait_for_lock(&self, pid: i32) -> bool {
        tokio::time::timeout(Duration::from_secs(4), async {
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
        .is_ok()
    }

    async fn wait_until(&self, deadline: i64) {
        tokio::time::timeout(Duration::from_secs(12), async {
            while clock(&self.db).await < deadline {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cross deadline while the row remains locked");
    }
}

struct NoExternalIo;

impl Mailer for NoExternalIo {
    async fn send_magic_link(&self, _email: &str, _token: &str) -> Result<(), StoreError> {
        panic!("refresh must not send mail")
    }
}

impl Dialer for NoExternalIo {
    async fn probe(&self, _target: &nntune_api::vault::ValidatedTarget, _tls: bool) -> bool {
        panic!("refresh must not dial a provider")
    }
}

impl AsyncDns for NoExternalIo {
    fn resolve<'a>(
        &'a self,
        _host: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<std::net::IpAddr>> + 'a>> {
        panic!("refresh must not resolve a provider")
    }
}

async fn handler_refresh(f: &Fixture, cookie: &str, retry_key: &str) -> Response {
    let store = nntune_api::worker_entry::session_retry_store_for_postgres_test(f.client().await);
    let cfg = handlers::Config::new(
        "app.example.test",
        "https://app.example.test",
        SIGNING_KEY.to_vec(),
        [0; 32],
        1,
        3600,
        false,
        false,
        None,
    )
    .unwrap();
    let ctx = handlers::Ctx {
        cfg: &cfg,
        store: &store,
        mailer: &NoExternalIo,
        dialer: &NoExternalIo,
        dns: &NoExternalIo,
        now: clock(&f.db).await as u64,
        source_ip: "127.0.0.1".into(),
    };
    handlers::handle(
        &ctx,
        &Request {
            method: "POST".into(),
            path: "/v1/session/refresh".into(),
            cookie: Some(cookie.into()),
            origin: Some(cfg.origin.clone()),
            authorization: None,
            body: serde_json::json!({"retry_key": retry_key})
                .to_string()
                .into_bytes(),
        },
    )
    .await
}

#[tokio::test]
async fn handler_through_production_store_preserves_retry_secret_isolation() {
    use sha2::Digest;

    let f = Fixture::new().await;
    let predecessor = token(&f.old);
    let cookie = format!("{COOKIE_NAME}={predecessor}");
    let key_bytes = [42; 32];
    let key = nntune_api::b64::encode(&key_bytes);
    let other_key = nntune_api::b64::encode(&[43; 32]);
    let first = handler_refresh(&f, &cookie, &key).await;
    assert_eq!(first.status, 200);
    let committed = row_state(&f.db).await;
    let mut digest = Sha256::new();
    digest.update(b"nntune-session-refresh-retry-v1\0");
    digest.update(key_bytes);
    digest.update(predecessor.as_bytes());
    assert_eq!(
        committed.2.as_deref(),
        Some(nntune_api::b64::encode(&digest.finalize()).as_str())
    );
    assert_eq!(committed.0, 1);
    assert!(committed.3.is_some() && committed.4.is_some());

    let rejected = handler_refresh(&f, &cookie, &other_key).await;
    assert_eq!(rejected.status, 401);
    assert!(rejected.set_cookie.is_none());
    assert_eq!(row_state(&f.db).await, committed);

    let recovered = handler_refresh(&f, &cookie, &key).await;
    assert_eq!(recovered.status, 200);
    let cookie_pair = |response: &Response| {
        response
            .set_cookie
            .as_deref()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    };
    assert_eq!(cookie_pair(&first), cookie_pair(&recovered));
    assert_eq!(row_state(&f.db).await, committed);
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
        assert!(f.wait_for_lock(pid).await);
    }
    lock.commit().await.unwrap();
    let first = joined(tasks.remove(0)).await;
    for task in tasks {
        assert_eq!(joined(task).await, first);
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
        assert!(f.wait_for_lock(pid).await);
    }
    lock.commit().await.unwrap();
    for (same_key, task) in tasks {
        assert_eq!(joined(task).await, same_key.then(|| first.clone()));
    }
}

#[tokio::test]
async fn real_adapter_mixed_keys_compete_for_first_rotation() {
    let f = Fixture::new().await;
    let mut locker = f.client().await;
    let lock = locker.transaction().await.unwrap();
    lock.query_one("SELECT id FROM sessions WHERE id='s1' FOR UPDATE", &[])
        .await
        .unwrap();
    let mut tasks = Vec::new();
    for digest in ["digest-a", "digest-b", "digest-a", "digest-b"] {
        let mut client = f.client().await;
        let pid = client
            .query_one("SELECT pg_backend_pid()", &[])
            .await
            .unwrap()
            .get(0);
        let old = f.old.clone();
        let next = f.next.clone();
        tasks.push((
            digest,
            tokio::spawn(async move {
                rotate_with_retry(&mut client, &old, &next, digest)
                    .await
                    .unwrap()
            }),
        ));
        assert!(
            f.wait_for_lock(pid).await,
            "each independent client must block before the first rotation"
        );
    }
    lock.commit().await.unwrap();
    let mut results = Vec::new();
    for (digest, task) in tasks {
        results.push((digest, joined(task).await));
    }
    let winner = results
        .iter()
        .find_map(|(digest, result)| result.as_ref().map(|claims| (*digest, claims.clone())))
        .expect("one group must rotate");
    for (digest, result) in results {
        assert_eq!(result, (digest == winner.0).then(|| winner.1.clone()));
    }
    let state = row_state(&f.db).await;
    assert_eq!(state.0, 1);
    assert_eq!(state.2.as_deref(), Some(winner.0));
}

#[tokio::test]
async fn real_adapter_preserves_worker_issued_at_across_split_clocks() {
    for skew in [1_i64, 30, -61] {
        let f = Fixture::new().await;
        let db_now = clock(&f.db).await;
        let worker_now = db_now + skew;
        let mut old = f.old.clone();
        old.created_at = worker_now as u64;
        old.issued_at = worker_now as u64;
        old.expires_at = (worker_now + 3600) as u64;
        let next = SessionClaims {
            rotation_counter: 1,
            ..old.clone()
        };
        f.db.execute(
            "UPDATE sessions SET created_at=$1, expires_at=$2 WHERE id='s1'",
            &[&worker_now, &(worker_now + 3600)],
        )
        .await
        .unwrap();
        let mut client = f.client().await;
        let before = clock(&f.db).await;
        let first = rotate_with_retry(&mut client, &old, &next, "bound-digest")
            .await
            .unwrap()
            .expect("first rotation");
        let after = clock(&f.db).await;
        let state = row_state(&f.db).await;
        assert_eq!(first.issued_at, next.issued_at);
        assert_eq!(state.3, Some(next.issued_at as i64));
        let claimed_at = state.4.expect("DB receipt clock");
        assert!((before..=after).contains(&claimed_at));
        if skew == -61 {
            assert_ne!(claimed_at, next.issued_at as i64);
        }
        if skew == 30 {
            // Retain an unambiguous split-clock oracle even if setup for the
            // one-second case crossed a clock tick on a busy runner.
            assert!(claimed_at < next.created_at as i64);
        }
        let signed = token(&first);
        assert_eq!(
            verify_session(SIGNING_KEY, &signed, worker_now as u64).unwrap(),
            first
        );
        let recovered = rotate_with_retry(&mut client, &old, &next, "bound-digest")
            .await
            .unwrap()
            .expect("bound retry");
        assert_eq!(recovered, first);
        assert_eq!(token(&recovered), signed);
        assert_eq!(
            row_state(&f.db).await,
            state,
            "retry must not strand or advance the counter"
        );
    }
}

#[tokio::test]
async fn real_adapter_uses_claim_order_not_request_timestamp_order() {
    let f = Fixture::new().await;
    let mut client = f.client().await;
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
    for _ in 0..ATTEMPTS {
        let f = Fixture::new().await;
        let mut client = f.client().await;
        rotate_with_retry(&mut client, &f.old, &f.next, "bound-digest")
            .await
            .unwrap()
            .unwrap();
        let claimed_at = clock(&f.db).await - 114;
        f.db.execute(
            "UPDATE sessions SET refresh_claimed_at=$1 WHERE id='s1'",
            &[&claimed_at],
        )
        .await
        .unwrap();
        let before = row_state(&f.db).await;
        let mut locker = f.client().await;
        let lock = locker.transaction().await.unwrap();
        // The holder makes no change; clock sampling must happen after the lock.
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
        let blocked = f.wait_for_lock(pid).await;
        let observed = clock(&f.db).await;
        if !blocked || observed >= claimed_at + 120 {
            lock.commit().await.unwrap();
            let _ = joined(task).await;
            continue;
        }
        assert!(
            observed < claimed_at + 120,
            "waiter must block while receipt is eligible"
        );
        f.wait_until(claimed_at + 120).await;
        lock.commit().await.unwrap();
        assert!(joined(task).await.is_none());
        assert_eq!(
            row_state(&f.db).await,
            before,
            "rejection cannot renew or change the receipt"
        );
        return;
    }
    panic!("could not establish a blocked waiter before the receipt deadline");
}

#[tokio::test]
async fn real_adapter_rechecks_expiry_after_lock_wait() {
    // Recovery of either expired predecessor or successor, plus a first
    // rotation whose candidate expires while its predecessor remains live.
    for scenario in 0..3 {
        let mut completed = false;
        for _ in 0..ATTEMPTS {
            let f = Fixture::new().await;
            let mut client = f.client().await;
            let expires = clock(&f.db).await + 8;
            let mut old = f.old.clone();
            let mut next = f.next.clone();
            if scenario == 0 {
                old.expires_at = expires as u64;
            } else {
                next.expires_at = expires as u64;
            }
            f.db.execute(
                "UPDATE sessions SET expires_at=$1 WHERE id='s1'",
                &[&(old.expires_at as i64)],
            )
            .await
            .unwrap();
            if scenario != 2 {
                let initial = rotate_with_retry(&mut client, &old, &next, "bound-digest")
                    .await
                    .unwrap();
                if initial.is_none() {
                    assert!(
                        clock(&f.db).await >= expires,
                        "eligible initial rotation unexpectedly rejected"
                    );
                    continue;
                }
            }
            let before = row_state(&f.db).await;
            if clock(&f.db).await + 2 >= expires {
                continue;
            }
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
            let blocked = f.wait_for_lock(pid).await;
            let observed = clock(&f.db).await;
            if !blocked || observed + 2 >= expires {
                lock.commit().await.unwrap();
                let _ = joined(task).await;
                continue;
            }
            assert!(
                observed + 2 < expires,
                "waiter must block with more than two seconds of eligibility"
            );
            f.wait_until(expires).await;
            lock.commit().await.unwrap();
            assert!(
                joined(task).await.is_none(),
                "scenario {scenario} must reject after expiry"
            );
            assert_eq!(
                row_state(&f.db).await,
                before,
                "scenario {scenario} must not change counter, expiry or receipt"
            );
            completed = true;
            break;
        }
        assert!(
            completed,
            "could not establish eligible blocked waiter for scenario {scenario}"
        );
    }
}
