//! Uses the production lifecycle transactions on CI's migrated, disposable
//! PostgreSQL. Each test owns a random synthetic user; audit failure injection
//! affects only a connection-local temporary audit table, never the shared one.
use super::*;
use crate::{listenbrainz::LinkInput, rotation::KekSource};
use std::time::Duration;
use tokio_postgres::{Config, NoTls};

const TOKEN_A: &str = "0b9e2f4a-1c3d-4e5f-8a6b-7c8d9e0f1a2b";
const TOKEN_B: &str = "1b9e2f4a-1c3d-4e5f-8a6b-7c8d9e0f1a2b";

struct Keys;
impl KekSource for Keys {
    fn kek(&self, version: u32) -> Option<&[u8; 32]> {
        (version == 1).then_some(&[7; 32])
    }
}

async fn client() -> Client {
    let config: Config = std::env::var("NNTUNE_TEST_DATABASE_URL")
        .expect("CI disposable database required; never silently skip")
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
        .batch_execute(
            "SET statement_timeout = '3s'; SET search_path = pg_temp, public;
         CREATE TEMP TABLE audit_log
         (LIKE public.audit_log INCLUDING DEFAULTS INCLUDING CONSTRAINTS)",
        )
        .await
        .unwrap();
    client
}

async fn user(client: &Client) -> String {
    let id: String = client
        .query_one("SELECT gen_random_uuid()::text", &[])
        .await
        .unwrap()
        .get(0);
    client
        .execute(
            "INSERT INTO users (id, email, created_at) VALUES ($1, $1 || '@example.test', 1000)",
            &[&id],
        )
        .await
        .unwrap();
    id
}

fn sealed(user_id: &str, username: &str, token: &str) -> StoredLink {
    LinkInput::new(username, token)
        .unwrap()
        .seal(&Keys, 1, user_id)
        .unwrap()
}

async fn assert_token(client: &Client, user_id: &str, expected: &str) {
    let link = read(client, user_id).await.unwrap().unwrap();
    // Do not include token plaintext in assertion diagnostics.
    assert!(link.open(&Keys, user_id).unwrap().expose() == expected);
}

#[tokio::test]
async fn link_relink_unlink_and_user_erasure_use_one_credential() {
    let mut db = client().await;
    let id = user(&db).await;
    let other = user(&db).await;
    let a = sealed(&id, "alice", TOKEN_A);
    let b = sealed(&id, "alice_new", TOKEN_B);
    assert!(read(&db, &id).await.unwrap().is_none());
    assert_eq!(
        replace(&mut db, &id, &a, "not an ip", 1000).await.unwrap(),
        FencedWriteOutcome::Written
    );
    assert_token(&db, &id, TOKEN_A).await;
    assert!(read(&db, &other).await.unwrap().is_none());
    assert!(replace(&mut db, &other, &a, "", 1001).await.is_err());
    assert_eq!(
        replace(&mut db, &id, &b, "", 1002).await.unwrap(),
        FencedWriteOutcome::Written
    );
    assert_token(&db, &id, TOKEN_B).await;
    assert_eq!(
        read(&db, &id)
            .await
            .unwrap()
            .unwrap()
            .summary()
            .username
            .as_deref(),
        Some("alice_new")
    );
    let count: i64 = db
        .query_one(
            "SELECT count(*) FROM listenbrainz_links WHERE user_id = $1",
            &[&id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 1);
    let audits = db
        .query(
            "SELECT action, details::text FROM audit_log ORDER BY at",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(audits.len(), 2);
    assert_eq!(audits[0].get::<_, String>(0), "listenbrainz.link");
    assert_eq!(audits[1].get::<_, String>(0), "listenbrainz.relink");
    assert!(audits.iter().all(|r| r.get::<_, String>(1) == "{}"));
    for now in [1003, 1004] {
        assert_eq!(
            unlink(&mut db, &id, "", now).await.unwrap(),
            FencedWriteOutcome::Written
        );
        assert!(read(&db, &id).await.unwrap().is_none());
    }
    replace(&mut db, &id, &a, "", 1005).await.unwrap();
    db.execute("DELETE FROM users WHERE id = $1", &[&id])
        .await
        .unwrap();
    assert!(
        read(&db, &id).await.unwrap().is_none(),
        "erasure must cascade"
    );
    assert_eq!(
        replace(&mut db, &id, &a, "", 1006).await.unwrap(),
        FencedWriteOutcome::NoRow
    );
    assert_eq!(
        unlink(&mut db, &id, "", 1007).await.unwrap(),
        FencedWriteOutcome::NoRow
    );
    db.execute("DELETE FROM users WHERE id = $1", &[&other])
        .await
        .unwrap();
}

#[tokio::test]
async fn failed_audit_rolls_back_relink_and_unlink() {
    let mut db = client().await;
    let id = user(&db).await;
    let a = sealed(&id, "alice", TOKEN_A);
    let b = sealed(&id, "bob", TOKEN_B);
    replace(&mut db, &id, &a, "", 1000).await.unwrap();
    db.batch_execute("ALTER TABLE pg_temp.audit_log ADD CHECK (action = 'listenbrainz.link')")
        .await
        .unwrap();
    assert!(replace(&mut db, &id, &b, "", 1001).await.is_err());
    assert_token(&db, &id, TOKEN_A).await;
    assert!(unlink(&mut db, &id, "", 1002).await.is_err());
    assert_token(&db, &id, TOKEN_A).await;
    let count: i64 = db
        .query_one("SELECT count(*) FROM audit_log", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        count, 1,
        "failed mutations must leave no audit or credential partials"
    );
    db.execute("DELETE FROM users WHERE id = $1", &[&id])
        .await
        .unwrap();
}

#[tokio::test]
async fn deletion_fence_serializes_even_an_initial_link() {
    let mut db = client().await;
    let id = user(&db).await;
    let a = sealed(&id, "alice", TOKEN_A);
    let mut blocker = client().await;
    let tx = blocker.transaction().await.unwrap();
    tx.query_one("SELECT id FROM users WHERE id = $1 FOR UPDATE", &[&id])
        .await
        .unwrap();
    let pending = replace(&mut db, &id, &a, "", 1000);
    tokio::pin!(pending);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut pending)
            .await
            .is_err(),
        "link must wait for the owner lock even when no link row exists"
    );
    tx.execute("UPDATE users SET deleting_at = now() WHERE id = $1", &[&id])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(pending.await.unwrap(), FencedWriteOutcome::AccountDeleting);
    // The pinned future holds db mutably until it drops; use the other
    // connection to observe and clean up the shared fixture.
    assert!(read(&blocker, &id).await.unwrap().is_none());
    assert_eq!(
        unlink(&mut blocker, &id, "", 1001).await.unwrap(),
        FencedWriteOutcome::AccountDeleting
    );
    blocker
        .execute("DELETE FROM users WHERE id = $1", &[&id])
        .await
        .unwrap();
}

#[tokio::test]
async fn malformed_replacement_cannot_remove_the_current_link() {
    let mut db = client().await;
    let id = user(&db).await;
    let a = sealed(&id, "alice", TOKEN_A);
    replace(&mut db, &id, &a, "", 1000).await.unwrap();
    let mut bad = a.clone();
    bad.envelope.ciphertext.pop();
    assert!(replace(&mut db, &id, &bad, "", 1001).await.is_err());
    assert_token(&db, &id, TOKEN_A).await;
    for version in [0, u32::MAX] {
        let mut bad = a.clone();
        bad.kek_version = version;
        assert!(replace(&mut db, &id, &bad, "", 1001).await.is_err());
    }
    assert_token(&db, &id, TOKEN_A).await;
    db.execute("DELETE FROM users WHERE id = $1", &[&id])
        .await
        .unwrap();
}
