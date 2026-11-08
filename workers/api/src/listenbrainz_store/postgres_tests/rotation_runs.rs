//! Private-schema, disposable Actions PostgreSQL only. All production entry
//! points are source-only; these tests never enable a runtime trigger.
use super::*;
use crate::listenbrainz_store::rotation_runs::{batch, open, reconcile};

struct Fixture {
    db: Client,
    schema: String,
}

impl Fixture {
    async fn new() -> Self {
        let db = client().await;
        let id: String = db
            .query_one("SELECT replace(gen_random_uuid()::text, '-', '')", &[])
            .await
            .unwrap()
            .get(0);
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
        let schema = format!("lb_runs_{id}");
        // LIKE does not copy foreign keys; add the private erasure relationship.
        db.batch_execute(&format!(
            "CREATE SCHEMA {schema};
             CREATE TABLE {schema}.users (LIKE public.users INCLUDING ALL);
             CREATE TABLE {schema}.listenbrainz_links
                 (LIKE public.listenbrainz_links INCLUDING ALL);
             ALTER TABLE {schema}.listenbrainz_links ADD FOREIGN KEY (user_id)
                 REFERENCES {schema}.users(id) ON DELETE CASCADE;
             CREATE TABLE {schema}.listenbrainz_rotation_runs
                 (LIKE public.listenbrainz_rotation_runs INCLUDING ALL);
             CREATE TABLE {schema}.audit_log (LIKE public.audit_log INCLUDING ALL);
             SET search_path = {schema}, pg_temp, public"
        ))
        .await
        .unwrap();
        Self { db, schema }
    }

    async fn other(&self) -> Client {
        let db = client().await;
        db.batch_execute(&format!(
            "SET search_path = {}, pg_temp, public",
            self.schema
        ))
        .await
        .unwrap();
        db
    }

    async fn link(&mut self, id: &str) -> StoredLink {
        self.db
            .execute(
                "INSERT INTO users (id, email, created_at) VALUES ($1, $1 || '@example.test', 1000)",
                &[&id],
            )
            .await
            .unwrap();
        let row = sealed(id, "alice", TOKEN_A);
        replace(&mut self.db, id, &row, "", 1000).await.unwrap();
        row
    }

    async fn audits(&self) -> i64 {
        self.db
            .query_one(
                "SELECT count(*) FROM audit_log WHERE action = 'listenbrainz.rewrap'",
                &[],
            )
            .await
            .unwrap()
            .get(0)
    }

    async fn cleanup(self) {
        self.db
            .batch_execute(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn open_is_idempotent_by_exact_id_and_excludes_every_same_env_target() {
    let mut f = Fixture::new().await;
    let mut other = f.other().await;
    let (a, b) = tokio::join!(
        open(&mut f.db, "a", "stage", 2, &Keys),
        open(&mut other, "b", "stage", 3, &Keys)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let (id, target) = if a.is_ok() { ("a", 2) } else { ("b", 3) };
    let saved = open(&mut f.db, id, "stage", target, &Keys).await.unwrap();
    assert_eq!(saved.checkpoint_seq, 0);
    assert!(saved.owner_cursor.is_none());
    assert!(open(
        &mut f.db,
        id,
        "stage",
        if target == 2 { 3 } else { 2 },
        &Keys
    )
    .await
    .is_err());
    assert!(open(&mut f.db, "different", "stage", target, &Keys)
        .await
        .is_err());
    assert!(open(&mut f.db, "independent", "other", 2, &Keys)
        .await
        .is_ok());
    for target in [0, u32::MAX, 4] {
        assert!(open(&mut f.db, "invalid", "new", target, &Keys)
            .await
            .is_err());
    }
    assert!(open(&mut f.db, "", "new", 2, &Keys).await.is_err());
    assert!(open(&mut f.db, "invalid", "", 2, &Keys).await.is_err());
    f.cleanup().await;
}

#[tokio::test]
async fn bounded_pages_commit_payload_audit_and_checkpoint_together() {
    for count in [0, 99, 100, 101] {
        let mut f = Fixture::new().await;
        let mut captured = Vec::new();
        for n in 0..count {
            captured.push(f.link(&format!("owner_{n:03}")).await);
        }
        open(&mut f.db, "run", "stage", 2, &Keys).await.unwrap();
        let first = batch(&mut f.db, "run", "stage", 2, 0, 100, &Keys, 1001)
            .await
            .unwrap();
        assert_eq!(first.scanned, i64::from(count.min(100)));
        assert_eq!(first.rewrapped, first.scanned);
        assert_eq!(first.checkpoint_seq, 1);
        assert_eq!(first.finished, count < 100);
        assert_eq!(f.audits().await, first.rewrapped);
        let saved = if first.finished {
            first
        } else {
            batch(&mut f.db, "run", "stage", 2, 1, 100, &Keys, 1002)
                .await
                .unwrap()
        };
        assert!(saved.finished);
        assert_eq!(saved.rewrapped, i64::from(count));
        assert_eq!(saved.scanned, saved.rewrapped);
        assert_eq!(f.audits().await, saved.rewrapped);
        for old in captured {
            let new = read(&f.db, &old.user_id).await.unwrap().unwrap();
            assert_eq!(new.kek_version, 2);
            assert_eq!(new.envelope.payload_nonce, old.envelope.payload_nonce);
            assert_eq!(new.envelope.ciphertext, old.envelope.ciphertext);
        }
        assert!(batch(
            &mut f.db,
            "run",
            "stage",
            2,
            saved.checkpoint_seq,
            100,
            &Keys,
            1003
        )
        .await
        .is_err());
        f.cleanup().await;
    }
}

#[tokio::test]
async fn poison_deletion_and_behind_cursor_links_remain_visible_for_a_new_sweep() {
    let mut f = Fixture::new().await;
    let ids = ["-a", "0", "A", "Z", "_", "a", "a-", "a_"];
    for id in ids {
        f.link(id).await;
    }
    f.db.execute("UPDATE users SET deleting_at = now() WHERE id = 'a-'", &[])
        .await
        .unwrap();
    f.db.execute("UPDATE listenbrainz_links SET dek_wrapped = decode(repeat('00', 48), 'hex') WHERE user_id = 'Z'", &[]).await.unwrap();
    open(&mut f.db, "run", "stage", 2, &Keys).await.unwrap();
    let first = batch(&mut f.db, "run", "stage", 2, 0, 3, &Keys, 1001)
        .await
        .unwrap();
    assert_eq!(first.owner_cursor.as_ref().unwrap().as_str(), "A");
    assert!(!first.finished);
    // A late eligible owner behind the checkpoint must wait for a fresh pass.
    f.link("-0").await;
    let end = batch(&mut f.db, "run", "stage", 2, 1, 100, &Keys, 1002)
        .await
        .unwrap();
    assert!(end.finished);
    assert_eq!(end.owner_cursor.as_ref().unwrap().as_str(), "a_");
    assert_eq!(
        (end.scanned, end.rewrapped, end.deferred, end.failed),
        (8, 6, 1, 1)
    );
    assert_eq!(read(&f.db, "-0").await.unwrap().unwrap().kek_version, 1);
    let next = open(&mut f.db, "next", "stage", 2, &Keys).await.unwrap();
    assert!(next.owner_cursor.is_none());
    let second = batch(&mut f.db, "next", "stage", 2, 0, 100, &Keys, 1003)
        .await
        .unwrap();
    assert_eq!(
        (
            second.scanned,
            second.rewrapped,
            second.deferred,
            second.failed
        ),
        (3, 1, 1, 1)
    );
    assert!(
        second.finished,
        "scan exhaustion must retain unresolved counts"
    );
    f.cleanup().await;
}

#[tokio::test]
async fn sql_audit_failure_after_one_row_rolls_back_every_effect() {
    let mut f = Fixture::new().await;
    for id in ["a", "b", "c"] {
        f.link(id).await;
    }
    open(&mut f.db, "run", "stage", 2, &Keys).await.unwrap();
    // First CAS+audit succeeds; the second audit aborts the entire transaction.
    f.db.batch_execute("ALTER TABLE audit_log ADD CONSTRAINT injected_failure CHECK (action <> 'listenbrainz.rewrap' OR user_id <> 'b')").await.unwrap();
    assert!(batch(&mut f.db, "run", "stage", 2, 0, 100, &Keys, 1001)
        .await
        .is_err());
    let saved = reconcile(&mut f.db, "run", "stage", 2).await.unwrap();
    assert_eq!((saved.checkpoint_seq, saved.scanned), (0, 0));
    assert!(saved.owner_cursor.is_none());
    assert!(!saved.finished);
    assert_eq!(f.audits().await, 0);
    for id in ["a", "b", "c"] {
        assert_eq!(read(&f.db, id).await.unwrap().unwrap().kek_version, 1);
    }
    f.db.batch_execute("ALTER TABLE audit_log DROP CONSTRAINT injected_failure")
        .await
        .unwrap();
    let saved = batch(&mut f.db, "run", "stage", 2, 0, 100, &Keys, 1002)
        .await
        .unwrap();
    assert_eq!((saved.scanned, saved.rewrapped), (3, 3));
    f.cleanup().await;
}

#[tokio::test]
async fn malformed_persisted_cursor_and_counter_overflow_cannot_commit() {
    let mut f = Fixture::new().await;
    f.link("a").await;
    open(&mut f.db, "run", "stage", 2, &Keys).await.unwrap();
    assert!(f
        .db
        .execute(
            "UPDATE listenbrainz_rotation_runs SET owner_cursor = ''",
            &[]
        )
        .await
        .is_err());
    // Simulate an out-of-contract persisted row, not a legal schema write.
    f.db.batch_execute("ALTER TABLE listenbrainz_rotation_runs DROP CONSTRAINT listenbrainz_rotation_runs_owner_cursor_check; UPDATE listenbrainz_rotation_runs SET owner_cursor = ''").await.unwrap();
    assert!(batch(&mut f.db, "run", "stage", 2, 0, 100, &Keys, 1001)
        .await
        .is_err());
    assert_eq!(f.audits().await, 0);
    f.db.execute(
        "UPDATE listenbrainz_rotation_runs SET owner_cursor = NULL, scanned = $1, rewrapped = $1",
        &[&i64::MAX],
    )
    .await
    .unwrap();
    assert!(batch(&mut f.db, "run", "stage", 2, 0, 100, &Keys, 1002)
        .await
        .is_err());
    assert_eq!(read(&f.db, "a").await.unwrap().unwrap().kek_version, 1);
    assert_eq!(f.audits().await, 0);
    let saved = reconcile(&mut f.db, "run", "stage", 2).await.unwrap();
    assert_eq!((saved.checkpoint_seq, saved.scanned), (0, i64::MAX));
    f.cleanup().await;
}

#[tokio::test]
async fn owner_lock_timeout_aborts_batch_without_partial_progress() {
    let mut f = Fixture::new().await;
    f.link("a").await;
    open(&mut f.db, "run", "stage", 2, &Keys).await.unwrap();
    let mut holder = f.other().await;
    let tx = holder.transaction().await.unwrap();
    tx.query_one("SELECT id FROM users WHERE id = 'a' FOR UPDATE", &[])
        .await
        .unwrap();
    // Production lock/statement controls must settle, not wait indefinitely.
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        batch(&mut f.db, "run", "stage", 2, 0, 100, &Keys, 1001),
    )
    .await
    .unwrap();
    assert!(result.is_err());
    tx.rollback().await.unwrap();
    assert_eq!(f.audits().await, 0);
    assert_eq!(read(&f.db, "a").await.unwrap().unwrap().kek_version, 1);
    let saved = reconcile(&mut f.db, "run", "stage", 2).await.unwrap();
    assert_eq!((saved.checkpoint_seq, saved.scanned), (0, 0));
    f.cleanup().await;
}

#[tokio::test]
async fn checkpoint_failure_rolls_back_prior_wraps_and_audits() {
    let mut f = Fixture::new().await;
    f.link("a").await;
    open(&mut f.db, "run", "stage", 2, &Keys).await.unwrap();
    f.db.batch_execute("ALTER TABLE listenbrainz_rotation_runs ADD CONSTRAINT injected_failure CHECK (checkpoint_seq = 0)").await.unwrap();
    assert!(batch(&mut f.db, "run", "stage", 2, 0, 100, &Keys, 1001)
        .await
        .is_err());
    assert_eq!(read(&f.db, "a").await.unwrap().unwrap().kek_version, 1);
    assert_eq!(f.audits().await, 0);
    assert_eq!(
        reconcile(&mut f.db, "run", "stage", 2)
            .await
            .unwrap()
            .checkpoint_seq,
        0
    );
    f.cleanup().await;
}

#[tokio::test]
async fn lock_fence_stale_sequence_and_wrong_identity_never_write() {
    let mut f = Fixture::new().await;
    f.link("a").await;
    open(&mut f.db, "run", "stage", 2, &Keys).await.unwrap();
    let mut holder = f.other().await;
    let tx = holder.transaction().await.unwrap();
    tx.query_one(
        "SELECT id FROM listenbrainz_rotation_runs WHERE id = 'run' FOR UPDATE",
        &[],
    )
    .await
    .unwrap();
    assert!(tokio::time::timeout(
        Duration::from_secs(2),
        batch(&mut f.db, "run", "stage", 2, 0, 100, &Keys, 1001)
    )
    .await
    .unwrap()
    .is_err());
    assert_eq!(f.audits().await, 0);
    assert_eq!(read(&f.db, "a").await.unwrap().unwrap().kek_version, 1);
    tx.rollback().await.unwrap();
    for (env, target, seq, limit) in [
        ("wrong", 2, 0, 100),
        ("stage", 3, 0, 100),
        ("stage", 2, 1, 100),
        ("stage", 2, 0, 0),
        ("stage", 2, 0, 101),
    ] {
        assert!(
            batch(&mut f.db, "run", env, target, seq, limit, &Keys, 1002)
                .await
                .is_err()
        );
    }
    f.db.execute(
        "UPDATE listenbrainz_rotation_runs SET checkpoint_seq = $1",
        &[&i64::MAX],
    )
    .await
    .unwrap();
    assert!(
        batch(&mut f.db, "run", "stage", 2, i64::MAX, 100, &Keys, 1003)
            .await
            .is_err()
    );
    assert_eq!(f.audits().await, 0);
    assert_eq!(read(&f.db, "a").await.unwrap().unwrap().kek_version, 1);
    f.cleanup().await;
}

#[tokio::test]
async fn discarded_commit_ack_is_reconciled_without_replaying_deltas_or_new_run_identity() {
    let mut f = Fixture::new().await;
    f.link("a").await;
    // Simulate an acknowledgement lost above the DB driver, not a network fault.
    drop(open(&mut f.db, "old", "stage", 2, &Keys).await);
    let opened = reconcile(&mut f.db, "old", "stage", 2).await.unwrap();
    assert_eq!(opened.checkpoint_seq, 0);
    drop(batch(&mut f.db, "old", "stage", 2, 0, 100, &Keys, 1001).await);
    open(&mut f.db, "new", "stage", 3, &Keys).await.unwrap();
    let mut fresh = f.other().await;
    let saved = reconcile(&mut fresh, "old", "stage", 2).await.unwrap();
    assert!(saved.finished);
    assert_eq!(
        (saved.checkpoint_seq, saved.scanned, saved.rewrapped),
        (1, 1, 1)
    );
    assert!(batch(&mut fresh, "old", "stage", 2, 0, 100, &Keys, 1002)
        .await
        .is_err());
    assert_eq!(f.audits().await, 1);
    assert_eq!(
        reconcile(&mut fresh, "new", "stage", 3)
            .await
            .unwrap()
            .checkpoint_seq,
        0
    );
    f.cleanup().await;
}

#[tokio::test]
async fn dropped_batch_future_rolls_back_before_authoritative_resume() {
    let mut f = Fixture::new().await;
    f.link("a").await;
    open(&mut f.db, "run", "stage", 2, &Keys).await.unwrap();
    let mut owner = f.other().await;
    let tx = owner.transaction().await.unwrap();
    tx.query_one("SELECT id FROM users WHERE id = 'a' FOR UPDATE", &[])
        .await
        .unwrap();
    let mut contender = f.other().await;
    {
        let mut pending = Box::pin(batch(&mut f.db, "run", "stage", 2, 0, 100, &Keys, 1001));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut pending)
                .await
                .is_err()
        );
        assert!(reconcile(&mut contender, "run", "stage", 2).await.is_err());
    }
    tx.rollback().await.unwrap();
    // This query settles behind the driver's queued rollback; elapsed time alone
    // is not evidence of rollback or permission to steal the transaction.
    f.db.simple_query("SELECT 1").await.unwrap();
    let saved = reconcile(&mut f.db, "run", "stage", 2).await.unwrap();
    assert_eq!((saved.checkpoint_seq, saved.scanned), (0, 0));
    assert_eq!(read(&f.db, "a").await.unwrap().unwrap().kek_version, 1);
    assert_eq!(f.audits().await, 0);
    assert_eq!(
        batch(&mut f.db, "run", "stage", 2, 0, 100, &Keys, 1002)
            .await
            .unwrap()
            .rewrapped,
        1
    );
    f.cleanup().await;
}

#[tokio::test]
async fn owner_locked_relink_is_reloaded_and_never_overwritten_by_stale_wrap() {
    let mut f = Fixture::new().await;
    let captured = f.link("a").await;
    open(&mut f.db, "run", "stage", 2, &Keys).await.unwrap();
    let mut writer = f.other().await;
    let tx = writer.transaction().await.unwrap();
    tx.query_one("SELECT id FROM users WHERE id = 'a' FOR UPDATE", &[])
        .await
        .unwrap();
    let new = sealed("a", "bob", TOKEN_B);
    tx.execute("UPDATE listenbrainz_links SET username = $1, dek_wrapped_nonce = $2, dek_wrapped = $3, payload_nonce = $4, ciphertext = $5 WHERE user_id = 'a'", &[&new.username.as_str(), &new.envelope.dek_wrapped_nonce.as_slice(), &new.envelope.dek_wrapped, &new.envelope.payload_nonce.as_slice(), &new.envelope.ciphertext]).await.unwrap();
    let mut pending = Box::pin(batch(&mut f.db, "run", "stage", 2, 0, 100, &Keys, 1001));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut pending)
            .await
            .is_err()
    );
    tx.commit().await.unwrap();
    let saved = pending.await.unwrap();
    // Depending on reload/owner-lock ordering, rotate the current row or defer
    // the captured one. Neither case may replace a same-version relink payload.
    assert_eq!(saved.scanned, 1);
    let actual = read(&f.db, "a").await.unwrap().unwrap();
    assert_eq!(actual.username, new.username);
    assert_eq!(actual.envelope.payload_nonce, new.envelope.payload_nonce);
    assert_eq!(actual.envelope.ciphertext, new.envelope.ciphertext);
    assert_ne!(actual.envelope.ciphertext, captured.envelope.ciphertext);
    assert!(actual.open(&Keys, "a").unwrap().expose() == TOKEN_B);
    f.cleanup().await;
}

#[tokio::test]
async fn erasure_during_owner_wait_never_resurrects_a_link() {
    let mut f = Fixture::new().await;
    f.link("a").await;
    open(&mut f.db, "run", "stage", 2, &Keys).await.unwrap();
    let mut writer = f.other().await;
    let tx = writer.transaction().await.unwrap();
    tx.query_one("SELECT id FROM users WHERE id = 'a' FOR UPDATE", &[])
        .await
        .unwrap();
    let mut pending = Box::pin(batch(&mut f.db, "run", "stage", 2, 0, 100, &Keys, 1001));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut pending)
            .await
            .is_err()
    );
    tx.execute("DELETE FROM users WHERE id = 'a'", &[])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let saved = pending.await.unwrap();
    assert_eq!((saved.scanned, saved.no_link), (1, 1));
    assert!(read(&f.db, "a").await.unwrap().is_none());
    assert_eq!(f.audits().await, 0);
    f.cleanup().await;
}
