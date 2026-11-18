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

#[path = "support/blocker_observer.rs"]
mod blocker_observer;
#[path = "support/commit_ack_fault.rs"]
mod commit_ack_fault;
#[path = "support/disposable_accounts.rs"]
mod disposable_accounts;
#[path = "support/private_account_fence.rs"]
mod private_account_fence;
#[path = "support/private_lease_commit_ack.rs"]
mod private_lease_commit_ack;
#[path = "support/private_lease_deletion.rs"]
mod private_lease_deletion;
#[path = "support/private_lease_ordering.rs"]
mod private_lease_ordering;
#[path = "support/private_lease_safety.rs"]
mod private_lease_safety;

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

// No arbitrary database URLs: this regression is wired only to a local
// throwaway service, never an application or production credential. Checks
// the driver's effective settings, including `hostaddr`, which the driver
// dials instead of `host` when set.
fn disposable_config(url: &str) -> Result<Config, &'static str> {
    let config: Config = url.parse().map_err(|_| "invalid test database URL")?;
    if config.get_dbname() != Some("nntune_ci") {
        return Err("test database must be disposable");
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
    Ok(config)
}

#[test]
fn disposable_config_rejects_non_local_targets() {
    let base = "postgresql://postgres:throwaway@localhost:5432/nntune_ci";
    for url in [
        base.to_owned(),
        "postgresql://postgres@127.0.0.1/nntune_ci".to_owned(),
        format!("{base}?hostaddr=127.0.0.1"),
    ] {
        assert!(disposable_config(&url).is_ok(), "allowed: {url}");
    }
    for url in [
        format!("{base}?hostaddr=203.0.113.10"),
        format!("{base}?hostaddr=127.0.0.1,203.0.113.10"),
        format!("{base}?host=remote.example.com"),
        format!("{base}?host=%2Fvar%2Frun%2Fpostgresql"),
        format!("{base}?dbname=application"),
        "postgresql://postgres@remote.example.com/nntune_ci".to_owned(),
    ] {
        assert!(disposable_config(&url).is_err(), "must refuse: {url}");
    }
}

impl Fixture {
    async fn new() -> Self {
        let url = std::env::var("NNTUNE_TEST_DATABASE_URL")
            .expect("NNTUNE_TEST_DATABASE_URL must name CI's disposable database");
        let config = disposable_config(&url).expect("scratch database configuration");
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

async fn lease_connection() -> Client {
    let db = disposable_accounts::connect().await;
    db.batch_execute("SET search_path TO pg_temp, public")
        .await
        .unwrap();
    db
}

#[tokio::test]
async fn production_lease_connection_seam_and_adapter() {
    use nntune_api::handlers::Store;
    use nntune_api::playback::LeaseAcquire::{Acquired, Held};
    use nntune_api::worker_entry::{
        acquire_playback_lease_on, session_retry_store_for_postgres_test,
    };

    let accounts = disposable_accounts::Accounts::new(2).await;
    let user = accounts.id(0);
    let mut db = lease_connection().await;
    let tx = db.transaction().await.unwrap();
    assert_eq!(
        acquire_playback_lease_on(&tx, user, "s1", "t1", "p1", 10, 55, false)
            .await
            .unwrap(),
        Acquired { epoch: 1 }
    );
    tx.rollback().await.unwrap();
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM public.playback_leases WHERE user_id=$1",
            &[&user]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    assert_eq!(
        acquire_playback_lease_on(&db, user, "s1", "t1", "p1", 10, 55, false)
            .await
            .unwrap(),
        Acquired { epoch: 1 }
    );
    let store = session_retry_store_for_postgres_test(lease_connection().await);
    assert_eq!(
        store
            .acquire_playback_lease(user, "s2", "t2", "p2", 20, 65, false)
            .await
            .unwrap(),
        Held {
            retry_after_secs: 35
        }
    );
    let row = db.query_one("SELECT session_id, track_id, play_id, epoch, expires_at, updated_at FROM public.playback_leases WHERE user_id=$1", &[&user]).await.unwrap();
    assert_eq!(
        (
            row.get::<_, String>(0),
            row.get::<_, String>(1),
            row.get::<_, String>(2),
            row.get::<_, i64>(3),
            row.get::<_, i64>(4),
            row.get::<_, i64>(5)
        ),
        ("s1".into(), "t1".into(), "p1".into(), 1, 55, 10)
    );
    assert_eq!(
        acquire_playback_lease_on(&db, user, "s2", "t2", "p2", 20, 65, true)
            .await
            .unwrap(),
        Acquired { epoch: 2 }
    );
    let store = session_retry_store_for_postgres_test(lease_connection().await);
    assert_eq!(
        store
            .acquire_playback_lease(user, "s3", "t3", "p3", 65, 100, false)
            .await
            .unwrap(),
        Acquired { epoch: 3 }
    );
    assert_eq!(
        acquire_playback_lease_on(&db, user, "s3", "t4", "p4", 66, 110, false)
            .await
            .unwrap(),
        Acquired { epoch: 4 }
    );
    assert!(
        acquire_playback_lease_on(&db, user, "s3", "t4", "p4", u64::MAX, 110, false)
            .await
            .is_err()
    );
    assert!(
        acquire_playback_lease_on(&db, user, "", "t4", "p4", 67, 111, true)
            .await
            .is_err()
    );
    db.execute(
        "UPDATE public.users SET deleting_at=now() WHERE id=$1",
        &[&user],
    )
    .await
    .unwrap();
    assert_eq!(
        acquire_playback_lease_on(&db, user, "s5", "t5", "p5", 67, 112, true)
            .await
            .unwrap(),
        Held {
            retry_after_secs: 43
        }
    );
    let row = db.query_one("SELECT session_id, track_id, play_id, epoch, expires_at, updated_at FROM public.playback_leases WHERE user_id=$1", &[&user]).await.unwrap();
    assert_eq!(
        (
            row.get::<_, String>(0),
            row.get::<_, String>(1),
            row.get::<_, String>(2),
            row.get::<_, i64>(3),
            row.get::<_, i64>(4),
            row.get::<_, i64>(5)
        ),
        ("s3".into(), "t4".into(), "p4".into(), 4, 110, 66)
    );
    let deleted_user = accounts.id(1);
    db.execute(
        "UPDATE public.users SET deleting_at=now() WHERE id=$1",
        &[&deleted_user],
    )
    .await
    .unwrap();
    assert_eq!(
        acquire_playback_lease_on(&db, deleted_user, "s1", "t1", "p1", 1, 50, false)
            .await
            .unwrap(),
        Held {
            retry_after_secs: 0
        }
    );
    let store = session_retry_store_for_postgres_test(lease_connection().await);
    assert!(store
        .acquire_playback_lease(user, "s1", "t1", "p1", u64::MAX, 110, false)
        .await
        .is_err());
    db.batch_execute("CREATE TEMP TABLE playback_leases (LIKE public.playback_leases); CREATE UNIQUE INDEX ON playback_leases(user_id)")
        .await
        .unwrap();
    db.execute(
        "INSERT INTO playback_leases VALUES ($1, 's1', 't1', 'p1', -1, 110, 1)",
        &[&user],
    )
    .await
    .unwrap();
    assert!(
        acquire_playback_lease_on(&db, user, "s2", "t2", "p2", 1, 50, false)
            .await
            .is_err()
    );
    accounts.cleanup().await;
}

#[tokio::test]
async fn private_aware_acquisition_commits_and_rolls_back() {
    use nntune_api::playback::LeaseAcquire::{Acquired, Held};
    use nntune_api::worker_entry::private_aware_lease_for_postgres_test as acquire;

    let accounts = disposable_accounts::Accounts::new(2).await;
    let user = accounts.id(0).to_owned();
    let other = accounts.id(1).to_owned();
    let db = &accounts.observer;
    let result: Result<(), String> = tokio::time::timeout(Duration::from_secs(30), async {
        let now: i64 = db.query_one("SELECT extract(epoch from statement_timestamp())::bigint", &[]).await.map_err(|e| e.to_string())?.get(0);
        db.execute("INSERT INTO public.private_sessions(user_id,session_id,expires_at) VALUES ($1,'private',$2),($1,'competitor',$2),($1,'expired',$3),($4,'other',$2)", &[&user, &(now + 300), &(now - 1), &other]).await.map_err(|e| e.to_string())?;
        for (session, play, expected_private) in [("missing", "a", false), ("expired", "b", false), ("other", "c", false), ("private", "d", true)] {
            let result = acquire(disposable_accounts::connect_guarded().await.map_err(|e| e.to_string())?, &user, session, "track", play, now as u64, now as u64 + 45, true).await.map_err(|_| "acquire failed")?;
            if result != (Acquired { epoch: match play { "a" => 1, "b" => 2, "c" => 3, _ => 4 } }, expected_private) { return Err(format!("wrong acquisition: {result:?}")); }
            let key = format!("play:{user}:track:{play}");
            let marked: bool = db.query_one("SELECT EXISTS(SELECT 1 FROM public.private_plays WHERE user_id=$1 AND play_key=$2)", &[&user, &key]).await.map_err(|e| e.to_string())?.get(0);
            if marked != expected_private { return Err("wrong persisted classification".into()); }
        }
        let held = acquire(disposable_accounts::connect_guarded().await.map_err(|e| e.to_string())?, &user, "competitor", "track", "refused", now as u64 + 1, now as u64 + 46, false).await.map_err(|_| "refusal failed")?;
        if held != (Held { retry_after_secs: 44 }, false) { return Err(format!("wrong refusal: {held:?}")); }
        let refused = format!("play:{user}:track:refused");
        let count: i64 = db.query_one("SELECT count(*) FROM public.private_plays WHERE user_id=$1 AND play_key=$2", &[&user, &refused]).await.map_err(|e| e.to_string())?.get(0);
        if count != 0 { return Err("refused mark persisted".into()); }
        let row = db.query_one("SELECT session_id,play_id,epoch FROM public.playback_leases WHERE user_id=$1", &[&user]).await.map_err(|e| e.to_string())?;
        if (row.get::<_, String>(0), row.get::<_, String>(1), row.get::<_, i64>(2)) != ("private".into(), "d".into(), 4) { return Err("incumbent changed".into()); }
        db.execute("UPDATE public.users SET deleting_at=now() WHERE id=$1", &[&user]).await.map_err(|e| e.to_string())?;
        for id in [&user, &format!("{user}-missing")] {
            if acquire(disposable_accounts::connect_guarded().await.map_err(|e| e.to_string())?, id, "private", "track", "e", now as u64, now as u64 + 45, true).await.is_ok() { return Err("missing/deleting owner accepted".into()); }
        }
        Ok(())
    }).await.unwrap_or_else(|_| Err("acquisition test timed out".into()));
    accounts.cleanup().await;
    result.unwrap();
}

#[tokio::test]
async fn private_aware_acquisition_errors_preserve_incumbent_and_marks() {
    use nntune_api::playback::LeaseAcquire::Acquired;
    use nntune_api::worker_entry::private_aware_lease_for_postgres_test as acquire;
    let accounts = disposable_accounts::Accounts::new(2).await;
    let user = accounts.id(0).to_owned();
    let other = accounts.id(1).to_owned();
    let db = &accounts.observer;
    let tag = format!("privacy_fault_{}", user.replace('-', ""));
    let result: Result<(), String> = tokio::time::timeout(Duration::from_secs(30), async {
        let now: i64 = db.query_one("SELECT extract(epoch from statement_timestamp())::bigint", &[]).await.map_err(|e| e.to_string())?.get(0);
        db.execute("INSERT INTO public.private_sessions(user_id,session_id,expires_at) VALUES ($1,'private',$2)", &[&user, &(now + 300)]).await.map_err(|e| e.to_string())?;
        if acquire(disposable_accounts::connect_guarded().await.map_err(|e| e.to_string())?, &user, "private", "track", "incumbent", now as u64, now as u64 + 45, false).await.map_err(|_| "seed failed")? != (Acquired { epoch: 1 }, true) { return Err("wrong seed".into()); }
        let other_key = format!("play:{other}:track:existing");
        db.execute("INSERT INTO public.private_plays(user_id,play_key,marked_at) VALUES ($1,$2,$3)", &[&other, &other_key, &now]).await.map_err(|e| e.to_string())?;
        db.batch_execute(&format!("CREATE FUNCTION public.{tag}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.user_id = '{user}' AND NEW.play_key LIKE '%:mark-error' THEN RAISE EXCEPTION 'synthetic mark failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER {tag} BEFORE INSERT ON public.private_plays FOR EACH ROW EXECUTE FUNCTION public.{tag}()"))
            .await.map_err(|e| e.to_string())?;
        // This role can take the account fence but cannot read private_sessions.
        // Probe both permissions so an earlier failure cannot masquerade as the query fault.
        db.batch_execute(&format!("CREATE ROLE {tag} NOLOGIN; GRANT USAGE ON SCHEMA public TO {tag}; GRANT SELECT, UPDATE ON public.users TO {tag}"))
            .await.map_err(|e| e.to_string())?;
        let restricted = disposable_accounts::connect_guarded().await.map_err(|e| e.to_string())?;
        restricted.batch_execute(&format!("SET ROLE {tag}")).await.map_err(|e| e.to_string())?;
        restricted.query(nntune_api::account_deletion::PRIVATE_SESSION_ACCOUNT_FENCE_SQL, &[&user]).await.map_err(|e| e.to_string())?;
        let error = restricted.query(nntune_api::preference_rows::private_session_active_statement(), &[&user, &"private"]).await.err().ok_or("privacy query unexpectedly allowed")?;
        if error.code() != Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE) { return Err("unexpected privacy query fault".into()); }
        if acquire(restricted, &user, "private", "track", "query-error", now as u64, now as u64 + 45, true).await.is_ok() { return Err("query error accepted".into()); }
        for (play, expiry) in [("mark-error", now as u64 + 45), ("lease-overflow", u64::MAX), ("lease-sql-error", 253_402_300_800)] {
            if acquire(disposable_accounts::connect_guarded().await.map_err(|e| e.to_string())?, &user, "private", "track", play, now as u64, expiry, true).await.is_ok() { return Err(format!("{play} error accepted")); }
        }
        let row = db.query_one("SELECT session_id,play_id,epoch FROM public.playback_leases WHERE user_id=$1", &[&user]).await.map_err(|e| e.to_string())?;
        if (row.get::<_, String>(0), row.get::<_, String>(1), row.get::<_, i64>(2)) != ("private".into(), "incumbent".into(), 1) { return Err("error changed incumbent".into()); }
        let rows = db.query("SELECT user_id,play_key FROM public.private_plays WHERE user_id=ANY($1) ORDER BY user_id,play_key", &[&vec![&user, &other]]).await.map_err(|e| e.to_string())?;
        let marks: Vec<(String, String)> = rows.iter().map(|r| (r.get(0), r.get(1))).collect();
        if marks.len() != 2 || !marks.contains(&(user.clone(), format!("play:{user}:track:incumbent"))) || !marks.contains(&(other.clone(), other_key)) { return Err("error changed retained marks".into()); }
        Ok(())
    }).await.unwrap_or_else(|_| Err("fault test timed out".into()));
    let cleanup = db.batch_execute(&format!("DROP TRIGGER IF EXISTS {tag} ON public.private_plays; DROP FUNCTION IF EXISTS public.{tag}(); DO $$ BEGIN IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{tag}') THEN DROP OWNED BY {tag}; DROP ROLE {tag}; END IF; END $$")) .await;
    accounts.cleanup().await;
    cleanup.unwrap();
    result.unwrap();
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
        stripe: &nntune_api::stripe::NoStripeTransport,
    };
    handlers::handle(
        &ctx,
        &Request {
            method: "POST".into(),
            path: "/v1/session/refresh".into(),
            cookie: Some(cookie.into()),
            origin: Some(cfg.origin.clone()),
            authorization: None,
            stripe_signature: None,
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
