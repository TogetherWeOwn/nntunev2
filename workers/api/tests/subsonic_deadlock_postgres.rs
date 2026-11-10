//! Session reauthentication overlapping compatibility credential issuance.
//! CI disposable PostgreSQL only: `--features postgres-tests
//! --test subsonic_deadlock_postgres`. Real schema (migrations 0002, 0006 and
//! 0037 in a private schema), so the actual `sessions.user_id` foreign key is
//! exercised. Both contenders run the production `Store` helpers through
//! separate single-use stores on separate backends.
//!
//! The reauthentication transaction claims the live session row (a blocking
//! row update) and then inserts its replacement, whose FK check takes
//! `FOR KEY SHARE` on the user row. Compatibility issuance takes its owner
//! lock first and then checks the session row `FOR SHARE`. The owner lock
//! must therefore stay compatible with the FK check: `FOR NO KEY UPDATE`
//! conflicts with owner serialization, the non-key `deleting_at` update and
//! the cancellation `FOR UPDATE`, but not with `FOR KEY SHARE`. A plain
//! `FOR UPDATE` here blocks the FK check and closes a lock cycle against
//! the session row lock, so PostgreSQL aborts one side with a deadlock.
//!
//! Overlap control reuses the advisory-lock pause-trigger pattern of
//! `credential_session_postgres`: the reauthentication INSERT is paused
//! after its session claim, and `pg_blocking_pids` proves issuance is
//! waiting before the pause is released. Read-committed evaluation then
//! refuses issuance against the revoked session; both sides settle with no
//! error and no credential row.
//!
//! Pre-fix negative control (limit): this pre-fix runtime abort is inferred
//! from the static lock trace and the PostgreSQL lock-compatibility matrix
//! rather than executed locally, per the Actions-first verification rule.
//! On the unfixed `FOR UPDATE` text the same choreography yields a
//! `40P01` deadlock abort against one contender, which fails the
//! both-`Ok` assertions below.
use nntune_api::handlers::Store;
use nntune_api::native_rest::Pepper;
use nntune_api::session::{ReauthenticatedSession, SessionClaims, SessionRecord};
use nntune_api::subsonic_auth::AuthMode;
use nntune_api::subsonic_lifecycle::{issue, IssuedCredential, PreparedCredential};
use nntune_api::worker_entry::session_retry_store_for_postgres_test;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Duration;
use tokio_postgres::{Client, Config, NoTls};

static SCHEMA: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    config: Config,
    schema: String,
    lock_key: i64,
    db: Client,
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

/// Same local-disposable guards as the sibling PostgreSQL fixtures: wired
/// only to a throwaway database, never an application or production target.
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
    assert!(disposable_config(base).is_ok());
    for url in [
        format!("{base}?hostaddr=203.0.113.10"),
        format!("{base}?host=remote.example.com"),
        format!("{base}?dbname=application"),
        "postgresql://postgres@remote.example.com/nntune_ci".to_owned(),
    ] {
        assert!(disposable_config(&url).is_err(), "must refuse: {url}");
    }
}

async fn pid(client: &Client) -> i32 {
    client
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0)
}

/// Bounded receive of a contender result; the threads cannot be killed, so
/// expiry is an honest failure rather than an abort.
async fn joined<T>(rx: Receiver<T>) -> T {
    let deadline = tokio::time::sleep(Duration::from_secs(12));
    tokio::pin!(deadline);
    loop {
        match rx.try_recv() {
            Ok(result) => return result,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(10)) => {}
                    _ = &mut deadline => panic!("contender did not finish after lock release"),
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                panic!("contender task ended without a result")
            }
        }
    }
}

/// Per-contender runtime. `PgStore` is not `Send`, so each store is built
/// inside its own thread and only the (Send) client crosses the boundary.
fn contender_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("contender runtime")
}

fn claims() -> SessionClaims {
    SessionClaims {
        session_id: "s1".into(),
        user_id: "u1".into(),
        created_at: 1,
        issued_at: 1,
        expires_at: 99_999,
        rotation_counter: 0,
    }
}

fn prepared() -> IssuedCredential {
    issue(
        "u1",
        "synthetic client",
        AuthMode::ApiKey,
        120,
        &Pepper {
            version: 1,
            bytes: &[7; 32],
        },
        1,
        &[8; 32],
    )
    .unwrap()
}

fn replacement() -> ReauthenticatedSession {
    ReauthenticatedSession {
        token: String::new(),
        new_record: SessionRecord {
            id: "s1r".into(),
            user_id: "u1".into(),
            created_at: 120,
            expires_at: 99_999,
            rotation_counter: 0,
            revoked: false,
        },
        revoked_session_id: "s1".into(),
        revoked_user_id: "u1".into(),
        revoked_rotation_counter: 0,
    }
}

impl Fixture {
    /// Real migrations 0002 (auth, with the actual sessions FK), 0006
    /// (deletion marker) and 0037 (compatibility credentials) in a private
    /// schema. One live owner u1 with one live session s1.
    async fn new() -> Self {
        let url = std::env::var("NNTUNE_TEST_DATABASE_URL")
            .expect("NNTUNE_TEST_DATABASE_URL must name CI's disposable database");
        let config = disposable_config(&url).expect("scratch database configuration");
        let n = SCHEMA.fetch_add(1, Ordering::Relaxed);
        let schema = format!("sublock_test_{}_{n}", std::process::id());
        let lock_key = ((std::process::id() as i64) << 20) | (n as i64 + 1_000_000);
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
            "../../../migrations/0006_account_deletion.sql"
        ))
        .await
        .unwrap();
        db.batch_execute(
            &include_str!("../../../migrations/0037_subsonic_credentials.sql")
                .replace("SET search_path = public, pg_catalog;", "")
                .replace("public.", &format!("{schema}.")),
        )
        .await
        .unwrap();
        db.batch_execute(
            "INSERT INTO users (id,email,created_at) VALUES ('u1','one@example.test',1);
             INSERT INTO sessions (id,user_id,created_at,expires_at,revoked)
             VALUES ('s1','u1',1,99999,false)",
        )
        .await
        .unwrap();
        Self {
            config,
            schema,
            lock_key,
            db,
        }
    }

    async fn client(&self) -> Client {
        connect(&self.config, &self.schema).await
    }

    /// Production-shaped pause on the replacement-session insert; waits on
    /// the fixture's advisory lock before the row (and its FK check) proceeds.
    async fn insert_pause_trigger(&self) {
        self.db
            .batch_execute(&format!(
                "CREATE OR REPLACE FUNCTION session_gate() RETURNS trigger LANGUAGE plpgsql AS
                 $$ BEGIN PERFORM pg_advisory_xact_lock({}); RETURN NEW; END $$;
                 DROP TRIGGER IF EXISTS session_gate ON sessions;
                 CREATE TRIGGER session_gate BEFORE INSERT ON sessions
                 FOR EACH ROW EXECUTE FUNCTION session_gate()"
            ))
            .await
            .unwrap();
    }

    async fn hold(&self) -> (Client, i32) {
        let client = self.client().await;
        client
            .execute("SELECT pg_advisory_lock($1)", &[&self.lock_key])
            .await
            .unwrap();
        let p = pid(&client).await;
        (client, p)
    }

    async fn release(&self, holder: &Client) {
        holder
            .execute("SELECT pg_advisory_unlock($1)", &[&self.lock_key])
            .await
            .unwrap();
    }

    /// Bounded wait until `waiter` is in a lock wait that `holder` blocks.
    async fn wait_blocked(&self, waiter: i32, holder: i32) {
        let seen = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let blocked: bool = self
                    .db
                    .query_one(
                        "SELECT COALESCE(a.wait_event_type = 'Lock'
                                AND $2 = ANY(pg_blocking_pids(a.pid)), false)
                         FROM (SELECT $1::int AS pid) p
                         LEFT JOIN pg_stat_activity a ON a.pid = p.pid",
                        &[&waiter, &holder],
                    )
                    .await
                    .unwrap()
                    .get(0);
                if blocked {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(seen.is_ok(), "backend {waiter} never waited on {holder}");
    }

    async fn compatibility_rows(&self) -> i64 {
        self.db
            .query_one(
                "SELECT count(*)::bigint FROM subsonic_credentials WHERE user_id = 'u1'",
                &[],
            )
            .await
            .unwrap()
            .get(0)
    }
}

#[tokio::test]
async fn reauthentication_overlapping_issuance_completes_without_deadlock() {
    let f = Fixture::new().await;
    f.insert_pause_trigger().await;
    let (holder, holder_pid) = f.hold().await;

    // Reauthentication starts first: claims s1 (row lock), then pauses
    // inside its replacement insert before the FK check.
    let reauth_client = f.client().await;
    let reauth_pid = pid(&reauth_client).await;
    let (reauth_tx, reauth_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = contender_runtime().block_on(async move {
            let store = session_retry_store_for_postgres_test(reauth_client);
            let repl = replacement();
            store.replace_session_after_reauthentication(&repl).await
        });
        let _ = reauth_tx.send(result);
    });
    f.wait_blocked(reauth_pid, holder_pid).await;

    // Issuance starts second: takes its owner lock, then waits on the
    // session row the reauthentication already claimed.
    let issue_client = f.client().await;
    let issue_pid = pid(&issue_client).await;
    let credential: PreparedCredential = prepared().prepared;
    let session = claims();
    let (issue_tx, issue_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = contender_runtime().block_on(async move {
            let store = session_retry_store_for_postgres_test(issue_client);
            store
                .insert_subsonic_credential(&credential, &session)
                .await
        });
        let _ = issue_tx.send(result);
    });
    f.wait_blocked(issue_pid, reauth_pid).await;

    f.release(&holder).await;
    assert!(
        joined(reauth_rx).await.unwrap(),
        "reauthentication must win"
    );
    assert!(
        !joined(issue_rx).await.unwrap(),
        "issuance must refuse the revoked session"
    );
    assert_eq!(
        f.compatibility_rows().await,
        0,
        "no credential may be minted"
    );
    assert_eq!(
        f.db
            .query_one("SELECT count(*)::bigint FROM sessions WHERE id='s1' AND revoked
                        UNION ALL SELECT count(*)::bigint FROM sessions WHERE id='s1r' AND NOT revoked", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        2,
        "old session revoked and replacement live"
    );
    f.db.batch_execute("DROP TRIGGER session_gate ON sessions")
        .await
        .unwrap();
}
