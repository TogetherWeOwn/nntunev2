//! Uses the production lifecycle transactions on CI's migrated, disposable
//! PostgreSQL. Each test owns a random synthetic user; audit failure injection
//! affects only a connection-local temporary audit table, never the shared one.
use super::*;
use crate::{listenbrainz::LinkInput, rotation::KekSource};
use std::time::Duration;
use tokio_postgres::{Config, NoTls};

mod rates;

const TOKEN_A: &str = "0b9e2f4a-1c3d-4e5f-8a6b-7c8d9e0f1a2b";
const TOKEN_B: &str = "1b9e2f4a-1c3d-4e5f-8a6b-7c8d9e0f1a2b";

struct Keys;
impl KekSource for Keys {
    fn kek(&self, version: u32) -> Option<&[u8; 32]> {
        match version {
            1 => Some(&[7; 32]),
            2 => Some(&[8; 32]),
            3 => Some(&[9; 32]),
            _ => None,
        }
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

async fn rewrap_audits(db: &Client) -> i64 {
    db.query_one(
        "SELECT count(*) FROM audit_log WHERE action = 'listenbrainz.rewrap'",
        &[],
    )
    .await
    .unwrap()
    .get(0)
}

async fn cleanup(db: &Client, id: &str) {
    db.execute("DELETE FROM users WHERE id = $1", &[&id])
        .await
        .unwrap();
}

/// Row is byte-identical to `expected` (never prints token material).
async fn assert_row(db: &Client, id: &str, expected: &StoredLink) {
    let row = read(db, id).await.unwrap().unwrap();
    assert!(row.username == expected.username);
    assert_eq!(row.kek_version, expected.kek_version);
    assert!(row.envelope.dek_wrapped_nonce == expected.envelope.dek_wrapped_nonce);
    assert!(row.envelope.dek_wrapped == expected.envelope.dek_wrapped);
    assert!(row.envelope.payload_nonce == expected.envelope.payload_nonce);
    assert!(row.envelope.ciphertext == expected.envelope.ciphertext);
}

#[tokio::test]
async fn rewrap_preserves_payload_and_created_at_and_audits_scalars() {
    let mut db = client().await;
    let id = user(&db).await;
    let a = sealed(&id, "alice", TOKEN_A);
    replace(&mut db, &id, &a, "", 1000).await.unwrap();
    let b = a.rewrap(&Keys, &id, 2).unwrap();
    assert_eq!(
        cas_rewrap(&mut db, &id, &a, &b, 2000).await.unwrap(),
        RewrapOutcome::Rewrapped
    );
    assert_row(&db, &id, &b).await;
    let stored = read(&db, &id).await.unwrap().unwrap();
    assert_eq!(stored.kek_version, 2);
    assert!(stored.envelope.payload_nonce == a.envelope.payload_nonce);
    assert!(stored.envelope.ciphertext == a.envelope.ciphertext);
    assert!(stored.open(&Keys, &id).unwrap().expose() == TOKEN_A);
    let created: i64 = db
        .query_one(
            "SELECT created_at FROM listenbrainz_links WHERE user_id = $1",
            &[&id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(created, 1000);
    let audits = db
        .query(
            "SELECT user_id, ok, source_ip, at, details::text FROM audit_log \
             WHERE action = 'listenbrainz.rewrap'",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].get::<_, String>(0), id);
    assert!(audits[0].get::<_, bool>(1));
    assert_eq!(
        audits[0].get::<_, String>(2),
        crate::vault::sanitize_source_ip(None)
    );
    assert_eq!(audits[0].get::<_, i64>(3), 2000);
    assert_eq!(audits[0].get::<_, String>(4), "{}");
    // A second claim holding the old capture now loses.
    let c = a.rewrap(&Keys, &id, 3).unwrap();
    assert_eq!(
        cas_rewrap(&mut db, &id, &a, &c, 2001).await.unwrap(),
        RewrapOutcome::ClaimLost
    );
    assert_row(&db, &id, &b).await;
    // Chained rotation from the fresh capture succeeds.
    let c = b.rewrap(&Keys, &id, 3).unwrap();
    assert_eq!(
        cas_rewrap(&mut db, &id, &b, &c, 2002).await.unwrap(),
        RewrapOutcome::Rewrapped
    );
    assert_eq!(rewrap_audits(&db).await, 2);
    cleanup(&db, &id).await;
}

#[tokio::test]
async fn stale_capture_loses_to_same_version_relink_and_unlink() {
    let mut db = client().await;
    let id = user(&db).await;
    let a = sealed(&id, "alice", TOKEN_A);
    replace(&mut db, &id, &a, "", 1000).await.unwrap();
    let stale = a.rewrap(&Keys, &id, 2).unwrap();
    // Same username and KEK version, different token.
    let b = sealed(&id, "alice", TOKEN_B);
    assert_eq!(b.kek_version, a.kek_version);
    replace(&mut db, &id, &b, "", 1001).await.unwrap();
    assert_eq!(
        cas_rewrap(&mut db, &id, &a, &stale, 2000).await.unwrap(),
        RewrapOutcome::ClaimLost
    );
    assert_row(&db, &id, &b).await;
    assert_token(&db, &id, TOKEN_B).await;
    unlink(&mut db, &id, "", 1002).await.unwrap();
    assert_eq!(
        cas_rewrap(&mut db, &id, &a, &stale, 2001).await.unwrap(),
        RewrapOutcome::ClaimLost,
        "an unlinked account must not be resurrected"
    );
    assert!(read(&db, &id).await.unwrap().is_none());
    assert_eq!(rewrap_audits(&db).await, 0);
    cleanup(&db, &id).await;
    assert_eq!(
        cas_rewrap(&mut db, &id, &a, &stale, 2002).await.unwrap(),
        RewrapOutcome::NoRow
    );
}

#[tokio::test]
async fn every_captured_field_mismatch_loses_without_writing() {
    let mut db = client().await;
    let id = user(&db).await;
    let a = sealed(&id, "alice", TOKEN_A);
    replace(&mut db, &id, &a, "", 1000).await.unwrap();
    let good = a.rewrap(&Keys, &id, 2).unwrap();
    type Edit = fn(&mut StoredLink);
    let edits: [(&str, Edit); 7] = [
        ("username", |l| {
            l.username = Username::parse("alice2").unwrap()
        }),
        ("username case", |l| {
            l.username = Username::parse("Alice").unwrap()
        }),
        ("wrap nonce", |l| l.envelope.dek_wrapped_nonce[0] ^= 1),
        ("wrapped dek", |l| l.envelope.dek_wrapped[0] ^= 1),
        ("payload nonce", |l| l.envelope.payload_nonce[0] ^= 1),
        ("ciphertext", |l| l.envelope.ciphertext[0] ^= 1),
        ("version", |l| l.kek_version = 3),
    ];
    for (name, edit) in edits {
        let mut captured = a.clone();
        edit(&mut captured);
        // Keep the replacement self-consistent so only the CAS can refuse it.
        let mut replacement = good.clone();
        replacement.kek_version = 4;
        if matches!(
            name,
            "username" | "username case" | "payload nonce" | "ciphertext"
        ) {
            edit(&mut replacement);
        }
        assert_eq!(
            cas_rewrap(&mut db, &id, &captured, &replacement, 2000)
                .await
                .unwrap(),
            RewrapOutcome::ClaimLost,
            "{name}"
        );
        assert_row(&db, &id, &a).await;
    }
    assert_eq!(rewrap_audits(&db).await, 0);
    cleanup(&db, &id).await;
}

#[tokio::test]
async fn invalid_rewrap_inputs_are_refused_before_any_write() {
    let mut db = client().await;
    let id = user(&db).await;
    let other = user(&db).await;
    let a = sealed(&id, "alice", TOKEN_A);
    replace(&mut db, &id, &a, "", 1000).await.unwrap();
    let good = a.rewrap(&Keys, &id, 2).unwrap();
    type Edit = fn(&mut StoredLink);
    let edits: [(&str, Edit); 9] = [
        ("username", |l| l.username = Username::parse("bob").unwrap()),
        ("payload nonce", |l| l.envelope.payload_nonce[0] ^= 1),
        ("ciphertext", |l| l.envelope.ciphertext[0] ^= 1),
        ("short ciphertext", |l| {
            l.envelope.ciphertext.pop();
        }),
        ("short wrap", |l| {
            l.envelope.dek_wrapped.pop();
        }),
        ("owner", |l| l.user_id = "someone-else".into()),
        ("same version", |l| l.kek_version = 1),
        ("zero version", |l| l.kek_version = 0),
        ("over INT4", |l| l.kek_version = u32::MAX),
    ];
    for (name, edit) in edits {
        let mut replacement = good.clone();
        edit(&mut replacement);
        assert!(
            cas_rewrap(&mut db, &id, &a, &replacement, 2000)
                .await
                .is_err(),
            "{name}"
        );
        assert_row(&db, &id, &a).await;
    }
    // Captured owner or shape wrong, or caller user mismatch.
    let mut captured = a.clone();
    captured.user_id = other.clone();
    assert!(cas_rewrap(&mut db, &id, &captured, &good, 2000)
        .await
        .is_err());
    let mut captured = a.clone();
    captured.envelope.ciphertext.pop();
    assert!(cas_rewrap(&mut db, &id, &captured, &good, 2000)
        .await
        .is_err());
    assert!(cas_rewrap(&mut db, &other, &a, &good, 2000).await.is_err());
    assert!(cas_rewrap(&mut db, &id, &a, &good, u64::MAX).await.is_err());
    assert_row(&db, &id, &a).await;
    assert!(read(&db, &other).await.unwrap().is_none());
    assert_eq!(rewrap_audits(&db).await, 0);
    cleanup(&db, &id).await;
    cleanup(&db, &other).await;
}

#[tokio::test]
async fn failed_audit_rolls_back_rewrap() {
    let mut db = client().await;
    let id = user(&db).await;
    let a = sealed(&id, "alice", TOKEN_A);
    replace(&mut db, &id, &a, "", 1000).await.unwrap();
    let b = a.rewrap(&Keys, &id, 2).unwrap();
    db.batch_execute("ALTER TABLE pg_temp.audit_log ADD CHECK (action <> 'listenbrainz.rewrap')")
        .await
        .unwrap();
    assert!(cas_rewrap(&mut db, &id, &a, &b, 2000).await.is_err());
    assert_row(&db, &id, &a).await;
    assert_token(&db, &id, TOKEN_A).await;
    assert_eq!(rewrap_audits(&db).await, 0);
    cleanup(&db, &id).await;
}

#[tokio::test]
async fn rewrap_respects_owner_lock_and_deletion_fence() {
    let mut db = client().await;
    let id = user(&db).await;
    let a = sealed(&id, "alice", TOKEN_A);
    replace(&mut db, &id, &a, "", 1000).await.unwrap();
    let b = a.rewrap(&Keys, &id, 2).unwrap();
    let mut blocker = client().await;
    let tx = blocker.transaction().await.unwrap();
    tx.query_one("SELECT id FROM users WHERE id = $1 FOR UPDATE", &[&id])
        .await
        .unwrap();
    let pending = cas_rewrap(&mut db, &id, &a, &b, 2000);
    tokio::pin!(pending);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut pending)
            .await
            .is_err(),
        "rewrap must wait for the owner lock"
    );
    tx.execute("UPDATE users SET deleting_at = now() WHERE id = $1", &[&id])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(pending.await.unwrap(), RewrapOutcome::AccountDeleting);
    assert_row(&blocker, &id, &a).await;
    assert_eq!(rewrap_audits(&blocker).await, 0);
    cleanup(&blocker, &id).await;
}

/// Isolated scan fixture: a connection-local `listenbrainz_links` table
/// shadows the shared one (search_path puts pg_temp first), so concurrent
/// tests' rows can never appear. Rows carry synthetic, well-shaped bytes.
async fn scan_client() -> Client {
    let db = client().await;
    db.batch_execute(
        "CREATE TEMP TABLE listenbrainz_links (LIKE public.listenbrainz_links INCLUDING ALL)",
    )
    .await
    .unwrap();
    db
}

fn scan_id(n: u32) -> String {
    format!("acct{n:018}")
}

async fn scan_put(db: &Client, id: &str, username: &str, version: i32) {
    db.execute(
        "INSERT INTO listenbrainz_links (user_id, username, kek_version, dek_wrapped_nonce, \
         dek_wrapped, payload_nonce, ciphertext, created_at) VALUES ($1, $2, $3, \
         decode(repeat('00', 12), 'hex'), decode(repeat('01', 48), 'hex'), \
         decode(repeat('02', 12), 'hex'), decode(repeat('03', 52), 'hex'), 1000)",
        &[&id, &username, &version],
    )
    .await
    .unwrap();
}

fn ids(page: &[OwnerCursor]) -> Vec<String> {
    page.iter().map(|c| c.as_str().to_owned()).collect()
}

#[tokio::test]
async fn rotation_scan_filters_orders_bounds_and_resumes_by_strict_cursor() {
    let db = scan_client().await;
    // Insert out of order; versions 1 and 2 are below target 3, 3 and 4 are not.
    for (n, v) in [(5, 1), (2, 2), (9, 3), (1, 1), (7, 4), (4, 2), (3, 1)] {
        scan_put(&db, &scan_id(n), "alice", v).await;
    }
    let all = rotation_candidates(&db, 3, None, 100).await.unwrap();
    assert_eq!(ids(&all), [1, 2, 3, 4, 5].map(scan_id));
    // Target below every version, and a lower target, exclude by strict `<`.
    assert!(rotation_candidates(&db, 1, None, 100)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        ids(&rotation_candidates(&db, 2, None, 100).await.unwrap()),
        [1, 3, 5].map(scan_id)
    );
    // Bound and keyset paging without gaps or repeats.
    let p1 = rotation_candidates(&db, 3, None, 2).await.unwrap();
    assert_eq!(ids(&p1), [1, 2].map(scan_id));
    let p2 = rotation_candidates(&db, 3, p1.last(), 2).await.unwrap();
    assert_eq!(ids(&p2), [3, 4].map(scan_id));
    let p3 = rotation_candidates(&db, 3, p2.last(), 2).await.unwrap();
    assert_eq!(ids(&p3), [5].map(scan_id));
    // Strict: a cursor equal to the last id yields nothing; a cursor that is
    // not itself a row (and not a candidate) still skips lower ids.
    assert!(rotation_candidates(&db, 3, p3.last(), 2)
        .await
        .unwrap()
        .is_empty());
    let between = OwnerCursor::parse(&scan_id(6)).unwrap();
    assert!(rotation_candidates(&db, 3, Some(&between), 100)
        .await
        .unwrap()
        .is_empty());
    let low = OwnerCursor::parse(&scan_id(2)).unwrap();
    assert_eq!(
        ids(&rotation_candidates(&db, 3, Some(&low), 100).await.unwrap()),
        [3, 4, 5].map(scan_id)
    );
}

#[tokio::test]
async fn rotation_scan_orders_opaque_ids_by_ascii_bytes() {
    let db = scan_client().await;
    // 22-char base64url account ids plus digit/upper/lower/_/- boundaries.
    // A locale collation would fold case and ignore punctuation; byte order
    // puts '-' < digits < uppercase < '_' < lowercase.
    let want = [
        "-AAAAAAAAAAAAAAAAAAAAA",
        "0AAAAAAAAAAAAAAAAAAAAA",
        "Zm9vYmFyYmF6cXV4MTIzNA",
        "a-b",
        "aB",
        "a_b",
        "ab",
        "dGVzdC1hY2NvdW50LTAwMQ",
    ];
    for id in [
        want[5], want[7], want[1], want[3], want[6], want[0], want[4], want[2],
    ] {
        scan_put(&db, id, "alice", 1).await;
    }
    let page = rotation_candidates(&db, 2, None, 100).await.unwrap();
    assert_eq!(ids(&page), want);
    // Keyset paging over the same ids, 3 at a time, with no gap or repeat.
    let mut after: Option<OwnerCursor> = None;
    let mut seen = Vec::new();
    loop {
        let p = rotation_candidates(&db, 2, after.as_ref(), 3)
            .await
            .unwrap();
        seen.extend(ids(&p));
        match p.last() {
            Some(last) if p.len() == 3 => after = Some(last.clone()),
            _ => break,
        }
    }
    assert_eq!(seen, want);
    // Case is significant: a cursor "aB" excludes it and lower ids only.
    let c = OwnerCursor::parse("aB").unwrap();
    assert_eq!(
        ids(&rotation_candidates(&db, 2, Some(&c), 100).await.unwrap()),
        ["a_b", "ab", "dGVzdC1hY2NvdW50LTAwMQ"]
    );
}

#[tokio::test]
async fn rotation_scan_rejects_bad_arguments_before_querying() {
    let db = scan_client().await;
    scan_put(&db, &scan_id(1), "alice", 1).await;
    // Every rejected call errors in the argument check; the fixture stays
    // intact and a valid call still succeeds afterwards.
    for (target, limit) in [(0, 10), (2, 0), (2, 101), (2, u32::MAX), (u32::MAX, 10)] {
        assert!(
            rotation_candidates(&db, target, None, limit).await.is_err(),
            "{target} {limit}"
        );
    }
    assert!(rotation_candidates(&db, 2, None, 100).await.is_ok());
    assert_eq!(
        ids(&rotation_candidates(&db, 2, None, 1).await.unwrap()),
        [scan_id(1)]
    );
}

#[tokio::test]
async fn rotation_scan_reads_ids_only_and_fails_closed_on_malformed_owner() {
    let db = scan_client().await;
    // A username the envelope decoder would reject must not matter: the scan
    // selects user_id only and never decodes a link.
    scan_put(&db, &scan_id(1), "bad name!", 1).await;
    assert!(read(&db, &scan_id(1)).await.is_err());
    assert_eq!(
        ids(&rotation_candidates(&db, 2, None, 10).await.unwrap()),
        [scan_id(1)]
    );
    // An id outside the credential-scope shape fails the scan closed instead
    // of being skipped (the table has no such CHECK).
    for bad in ["bad id!", "a.b"] {
        scan_put(&db, bad, "alice", 1).await;
        assert!(
            rotation_candidates(&db, 2, None, 10).await.is_err(),
            "{bad}"
        );
        db.execute("DELETE FROM listenbrainz_links WHERE user_id = $1", &[&bad])
            .await
            .unwrap();
    }
    assert!(rotation_candidates(&db, 2, None, 10).await.is_ok());
}
