//! Production credential/session helpers on CI's disposable PostgreSQL service.
//! Run with --features postgres-tests --test credential_session_postgres.
//! Missing test configuration fails; ordinary unit tests need no database.
//! Overlap tests pause the real helpers mid-transaction with production-shaped
//! triggers gated on an advisory lock, and prove each contender is waiting
//! (pg_stat_activity + pg_blocking_pids) before the pause is released.
use nntune_api::credential_session_store::{
    insert_native_credential, revoke_sessions_and_native_credentials,
};
use nntune_api::handlers::{NativeCredentialRow, StoreError};
use nntune_api::native_rest::{NativeRestRecord, Scope};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::task::JoinHandle;
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

async fn pid(client: &Client) -> i32 {
    client
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0)
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

// No arbitrary database URLs: wired only to a local throwaway service, never
// an application or production credential. Checks the driver's effective
// settings, including `hostaddr`, which the driver dials instead of `host`.
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

fn cred(id: &str, user: &str, hmac: &str) -> NativeCredentialRow {
    NativeCredentialRow {
        record: NativeRestRecord {
            id: id.into(),
            user_id: user.into(),
            key_prefix: "ntk_abcd".into(),
            key_hmac: hmac.into(),
            pepper_version: 1,
            scopes: vec![Scope::LibraryRead, Scope::StreamRead],
            created_at: 100,
            expires_at: 10_000,
            revoked_at: None,
        },
        label: "synthetic label".into(),
        created_from_ip: "203.0.113.7".into(),
        last_used_at: None,
    }
}

impl Fixture {
    /// Real migrations 0002 (auth), 0006 (deletion marker) and 0019 (native
    /// credentials) in a private schema. Users: u1, u2 live; u3 deleting.
    /// Sessions: s1, s1b (u1 live), sr (u1 revoked), s2 (u2), s3 (u3).
    async fn new() -> Self {
        let url = std::env::var("NNTUNE_TEST_DATABASE_URL")
            .expect("NNTUNE_TEST_DATABASE_URL must name CI's disposable database");
        let config = disposable_config(&url).expect("scratch database configuration");
        let n = SCHEMA.fetch_add(1, Ordering::Relaxed);
        let schema = format!("credsess_test_{}_{n}", std::process::id());
        let lock_key = ((std::process::id() as i64) << 20) | n as i64;
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
        db.batch_execute(include_str!(
            "../../../migrations/0019_native_rest_credentials.sql"
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
            "INSERT INTO users (id,email,created_at) VALUES
               ('u1','one@example.test',1),('u2','two@example.test',1);
             INSERT INTO users (id,email,created_at,deleting_at)
               VALUES ('u3','three@example.test',1,now());
             INSERT INTO sessions (id,user_id,created_at,expires_at,revoked) VALUES
               ('s1','u1',1,99999,false),('s1b','u1',1,99999,false),
               ('sr','u1',1,99999,true),('s2','u2',1,99999,false),
               ('s3','u3',1,99999,false)",
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

    /// Production-shaped trigger on the credential table. `pause` waits on the
    /// fixture's advisory lock; otherwise it raises (a failing credential write).
    async fn trigger(&self, event: &str, each: &str, pause: bool) {
        let body = if pause {
            format!("PERFORM pg_advisory_xact_lock({});", self.lock_key)
        } else {
            "RAISE EXCEPTION 'synthetic credential write failure';".into()
        };
        self.db
            .batch_execute(&format!(
                "CREATE OR REPLACE FUNCTION cred_gate() RETURNS trigger LANGUAGE plpgsql AS
                 $$ BEGIN {body} RETURN NEW; END $$;
                 DROP TRIGGER IF EXISTS cred_gate ON native_rest_credentials;
                 CREATE TRIGGER cred_gate {event} ON native_rest_credentials
                 {each} EXECUTE FUNCTION cred_gate()"
            ))
            .await
            .unwrap();
    }

    /// A client that holds the advisory lock the pause trigger waits on.
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

    async fn mint(
        &self,
        id: &str,
        user: &str,
        hmac: &str,
        session: &str,
    ) -> Result<(), StoreError> {
        let mut client = self.client().await;
        insert_native_credential(&mut client, &cred(id, user, hmac), session).await
    }

    async fn credentials(&self) -> Vec<(String, Option<i64>)> {
        self.db
            .query(
                "SELECT id, revoked_at FROM native_rest_credentials ORDER BY id",
                &[],
            )
            .await
            .unwrap()
            .iter()
            .map(|r| (r.get(0), r.get(1)))
            .collect()
    }

    async fn revoked_sessions(&self) -> Vec<String> {
        self.db
            .query("SELECT id FROM sessions WHERE revoked ORDER BY id", &[])
            .await
            .unwrap()
            .iter()
            .map(|r| r.get(0))
            .collect()
    }

    /// Every row the helpers lock must be immediately lockable by a stranger.
    async fn assert_unlocked(&self) {
        let mut other = self.client().await;
        let tx = other.transaction().await.unwrap();
        for table in ["users", "sessions", "native_rest_credentials"] {
            tx.query(&format!("SELECT 1 FROM {table} FOR UPDATE NOWAIT"), &[])
                .await
                .unwrap_or_else(|e| panic!("{table} still locked: {e}"));
        }
    }
}

fn rev(id: &str, at: i64) -> (String, Option<i64>) {
    (id.into(), Some(at))
}

fn live(id: &str) -> (String, Option<i64>) {
    (id.into(), None)
}

#[tokio::test]
async fn mint_persists_full_row_then_revoke_is_scoped_and_sticky() {
    let f = Fixture::new().await;
    f.mint("c1", "u1", "h1", "s1").await.unwrap();
    f.mint("c2", "u1", "h2", "s1b").await.unwrap();
    f.mint("c3", "u2", "h3", "s2").await.unwrap();
    let row =
        f.db.query_one(
            "SELECT user_id,label,key_prefix,key_hmac,pepper_version,scopes,created_at,
                    expires_at,last_used_at,revoked_at,created_from_ip
             FROM native_rest_credentials WHERE id='c1'",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "u1");
    assert_eq!(row.get::<_, String>(1), "synthetic label");
    assert_eq!(row.get::<_, String>(2), "ntk_abcd");
    assert_eq!(row.get::<_, String>(3), "h1");
    assert_eq!(row.get::<_, i32>(4), 1);
    assert_eq!(
        row.get::<_, Vec<String>>(5),
        vec![
            Scope::LibraryRead.as_str().to_string(),
            Scope::StreamRead.as_str().to_string()
        ]
    );
    assert_eq!((row.get::<_, i64>(6), row.get::<_, i64>(7)), (100, 10_000));
    assert_eq!(row.get::<_, Option<i64>>(8), None);
    assert_eq!(row.get::<_, Option<i64>>(9), None);
    assert_eq!(row.get::<_, String>(10), "203.0.113.7");

    // A credential revoked earlier keeps its timestamp and is not counted.
    f.db.execute(
        "UPDATE native_rest_credentials SET revoked_at=50 WHERE id='c2'",
        &[],
    )
    .await
    .unwrap();
    let mut client = f.client().await;
    let n = revoke_sessions_and_native_credentials(&mut client, "u1", 500)
        .await
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(
        f.credentials().await,
        vec![rev("c1", 500), rev("c2", 50), live("c3")]
    );
    assert_eq!(f.revoked_sessions().await, ["s1", "s1b", "sr"]);

    // Repeat revoke and empty owner are no-ops; later times never overwrite.
    for (owner, at) in [("u1", 900), ("", 900), ("nobody", 900)] {
        let n = revoke_sessions_and_native_credentials(&mut client, owner, at)
            .await
            .unwrap();
        assert_eq!(n, 0, "owner {owner:?}");
    }
    assert_eq!(
        f.credentials().await,
        vec![rev("c1", 500), rev("c2", 50), live("c3")]
    );
    assert_eq!(f.revoked_sessions().await, ["s1", "s1b", "sr"]);
}

#[tokio::test]
async fn revoke_then_mint_is_refused_and_leaves_no_credential() {
    let f = Fixture::new().await;
    let mut client = f.client().await;
    revoke_sessions_and_native_credentials(&mut client, "u1", 500)
        .await
        .unwrap();
    assert!(f.mint("c1", "u1", "h1", "s1").await.is_err());
    assert!(f.credentials().await.is_empty());
    f.assert_unlocked().await;
}

#[tokio::test]
async fn mint_requires_live_owned_session_and_non_deleting_owner() {
    let f = Fixture::new().await;
    for (user, session) in [
        ("u1", "missing"),
        ("u1", "sr"),
        ("u1", "s2"),
        ("u3", "s3"),
        ("missing", "s1"),
    ] {
        assert!(
            f.mint("c1", user, "h1", session).await.is_err(),
            "{user}/{session}"
        );
        assert!(f.credentials().await.is_empty(), "{user}/{session}");
        f.assert_unlocked().await;
    }
    f.mint("c1", "u1", "h1", "s1").await.unwrap();
}

#[tokio::test]
async fn failed_insert_rolls_back_and_releases_locks() {
    let f = Fixture::new().await;
    let mut client = f.client().await;
    insert_native_credential(&mut client, &cred("c1", "u1", "h1"), "s1")
        .await
        .unwrap();
    // Duplicate (pepper_version, key_hmac) fails after both fences are held.
    assert!(
        insert_native_credential(&mut client, &cred("c2", "u1", "h1"), "s1")
            .await
            .is_err()
    );
    f.assert_unlocked().await;
    // The same connection stays usable.
    insert_native_credential(&mut client, &cred("c2", "u1", "h2"), "s1")
        .await
        .unwrap();
    assert_eq!(f.credentials().await, vec![live("c1"), live("c2")]);
}

#[tokio::test]
async fn failure_after_session_update_rolls_back_both_updates() {
    let f = Fixture::new().await;
    f.mint("c1", "u1", "h1", "s1").await.unwrap();
    f.trigger("BEFORE UPDATE", "FOR EACH STATEMENT", false)
        .await;
    let mut client = f.client().await;
    assert!(
        revoke_sessions_and_native_credentials(&mut client, "u1", 500)
            .await
            .is_err()
    );
    assert_eq!(f.revoked_sessions().await, ["sr"], "session update leaked");
    assert_eq!(f.credentials().await, vec![live("c1")]);
    f.assert_unlocked().await;

    f.db.execute("DROP TRIGGER cred_gate ON native_rest_credentials", &[])
        .await
        .unwrap();
    let n = revoke_sessions_and_native_credentials(&mut client, "u1", 600)
        .await
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(f.credentials().await, vec![rev("c1", 600)]);
}

#[tokio::test]
async fn compatibility_failure_rolls_back_recovery_across_all_credential_surfaces() {
    let f = Fixture::new().await;
    f.mint("c1", "u1", "h1", "s1").await.unwrap();
    f.db.batch_execute(
        "INSERT INTO subsonic_credentials
               (user_id,subsonic_username,api_key_hmac,pepper_version,revoked_at) VALUES
               ('u1','compat_live',repeat('a',64),1,NULL),
               ('u1','compat_revoked',repeat('b',64),1,to_timestamp(50)),
               ('u2','compat_foreign',repeat('c',64),1,NULL);
             CREATE FUNCTION compatibility_revoke_failure() RETURNS trigger LANGUAGE plpgsql AS
               $$ BEGIN RAISE EXCEPTION 'synthetic compatibility update failure'; END $$;
             CREATE TRIGGER compatibility_revoke_failure BEFORE UPDATE ON subsonic_credentials
               FOR EACH STATEMENT EXECUTE FUNCTION compatibility_revoke_failure()",
    )
    .await
    .unwrap();
    let mut client = f.client().await;
    assert!(
        revoke_sessions_and_native_credentials(&mut client, "u1", 500)
            .await
            .is_err()
    );
    assert_eq!(f.revoked_sessions().await, ["sr"]);
    assert_eq!(f.credentials().await, vec![live("c1")]);
    let rows = f.db
        .query("SELECT subsonic_username, EXTRACT(EPOCH FROM revoked_at)::bigint FROM subsonic_credentials ORDER BY subsonic_username", &[])
        .await.unwrap();
    assert_eq!(
        rows.iter()
            .map(|r| r.get::<_, Option<i64>>(1))
            .collect::<Vec<_>>(),
        vec![None, None, Some(50)]
    );
    f.assert_unlocked().await;

    f.db.batch_execute("DROP TRIGGER compatibility_revoke_failure ON subsonic_credentials")
        .await
        .unwrap();
    assert_eq!(
        revoke_sessions_and_native_credentials(&mut client, "u1", 600)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        revoke_sessions_and_native_credentials(&mut client, "u1", 900)
            .await
            .unwrap(),
        0
    );
    assert_eq!(f.revoked_sessions().await, ["s1", "s1b", "sr"]);
    assert_eq!(f.credentials().await, vec![rev("c1", 600)]);
    let rows = f.db
        .query("SELECT subsonic_username, EXTRACT(EPOCH FROM revoked_at)::bigint FROM subsonic_credentials ORDER BY subsonic_username", &[])
        .await.unwrap();
    assert_eq!(
        rows.iter()
            .map(|r| r.get::<_, Option<i64>>(1))
            .collect::<Vec<_>>(),
        vec![None, Some(600), Some(50)]
    );
    f.assert_unlocked().await;
}

#[tokio::test]
async fn overlapping_mint_first_makes_revoke_wait_and_revoke_the_new_credential() {
    let f = Fixture::new().await;
    f.trigger("BEFORE INSERT", "FOR EACH ROW", true).await;
    let (holder, holder_pid) = f.hold().await;

    let mut minter = f.client().await;
    let mint_pid = pid(&minter).await;
    let mint = tokio::spawn(async move {
        insert_native_credential(&mut minter, &cred("c1", "u1", "h1"), "s1").await
    });
    f.wait_blocked(mint_pid, holder_pid).await;

    let mut revoker = f.client().await;
    let revoke_pid = pid(&revoker).await;
    let revoke = tokio::spawn(async move {
        revoke_sessions_and_native_credentials(&mut revoker, "u1", 500).await
    });
    f.wait_blocked(revoke_pid, mint_pid).await;

    f.release(&holder).await;
    joined(mint).await.unwrap();
    assert_eq!(joined(revoke).await.unwrap(), 1);
    assert_eq!(f.credentials().await, vec![rev("c1", 500)]);
    assert_eq!(f.revoked_sessions().await, ["s1", "s1b", "sr"]);
    f.assert_unlocked().await;
}

#[tokio::test]
async fn overlapping_revoke_first_makes_mint_wait_and_then_fail() {
    let f = Fixture::new().await;
    f.mint("c0", "u1", "h0", "s1").await.unwrap();
    f.trigger("BEFORE UPDATE", "FOR EACH STATEMENT", true).await;
    let (holder, holder_pid) = f.hold().await;

    let mut revoker = f.client().await;
    let revoke_pid = pid(&revoker).await;
    let revoke = tokio::spawn(async move {
        revoke_sessions_and_native_credentials(&mut revoker, "u1", 500).await
    });
    f.wait_blocked(revoke_pid, holder_pid).await;

    let mut minter = f.client().await;
    let mint_pid = pid(&minter).await;
    let mint = tokio::spawn(async move {
        insert_native_credential(&mut minter, &cred("c1", "u1", "h1"), "s1").await
    });
    f.wait_blocked(mint_pid, revoke_pid).await;

    f.release(&holder).await;
    assert_eq!(joined(revoke).await.unwrap(), 1);
    assert!(joined(mint).await.is_err());
    assert_eq!(f.credentials().await, vec![rev("c0", 500)]);
    f.assert_unlocked().await;
}
