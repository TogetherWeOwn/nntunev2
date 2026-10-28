//! One transaction owns the magic-link claim, session insert and commit.
//! Kept independent of the Worker runtime so failure-after-claim tests execute
//! the same orchestration against a real, injectable database transaction.

use crate::auth::{MagicLinkRecord, SingleUseClaim};
use crate::handlers::StoreError;
use crate::session::SessionRecord;

pub(crate) trait RedemptionTransaction: Sized {
    async fn claim(&self, magic: &MagicLinkRecord, now: u64) -> Result<usize, StoreError>;
    async fn insert_session(&self, session: &SessionRecord) -> Result<(), StoreError>;
    async fn commit(self) -> Result<(), StoreError>;
    async fn rollback(self) -> Result<(), StoreError>;
}

const INSERT_SESSION: &str =
    "INSERT INTO sessions (id, user_id, created_at, expires_at, rotation_counter, revoked) \
     VALUES ($1, $2, $3, $4, $5, FALSE)";

impl RedemptionTransaction for tokio_postgres::Transaction<'_> {
    async fn claim(&self, magic: &MagicLinkRecord, now: u64) -> Result<usize, StoreError> {
        let claim = SingleUseClaim::MagicLink { record: magic, now };
        let expires = i64::try_from(magic.expires_at).map_err(|_| StoreError)?;
        let now = i64::try_from(now).map_err(|_| StoreError)?;
        self.query(
            claim.statement(),
            &[
                &magic.token_hash,
                &magic.user_id,
                &magic.email,
                &expires,
                &now,
            ],
        )
        .await
        .map(|rows| rows.len())
        .map_err(|_| StoreError)
    }

    async fn insert_session(&self, session: &SessionRecord) -> Result<(), StoreError> {
        let created_at = i64::try_from(session.created_at).map_err(|_| StoreError)?;
        let expires_at = i64::try_from(session.expires_at).map_err(|_| StoreError)?;
        let counter = i32::try_from(session.rotation_counter).map_err(|_| StoreError)?;
        self.execute(
            INSERT_SESSION,
            &[
                &session.id,
                &session.user_id,
                &created_at,
                &expires_at,
                &counter,
            ],
        )
        .await
        .map(|_| ())
        .map_err(|_| StoreError)
    }

    async fn commit(self) -> Result<(), StoreError> {
        tokio_postgres::Transaction::commit(self)
            .await
            .map_err(|_| StoreError)
    }

    async fn rollback(self) -> Result<(), StoreError> {
        tokio_postgres::Transaction::rollback(self)
            .await
            .map_err(|_| StoreError)
    }
}

pub(crate) async fn redeem(
    transaction: impl RedemptionTransaction,
    magic: &MagicLinkRecord,
    now: u64,
    session: &SessionRecord,
) -> Result<bool, StoreError> {
    if session.user_id != magic.user_id {
        transaction.rollback().await?;
        return Err(StoreError);
    }
    match transaction.claim(magic, now).await {
        Ok(1) => {}
        Ok(_) => {
            transaction.rollback().await?;
            return Ok(false);
        }
        Err(_) => {
            transaction.rollback().await?;
            return Err(StoreError);
        }
    }
    if transaction.insert_session(session).await.is_err() {
        transaction.rollback().await?;
        return Err(StoreError);
    }
    // Drop also rolls back on cancellation. Never reset a spent token with a
    // compensating write, nor reopen it after an ambiguous COMMIT response.
    transaction.commit().await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{params, Connection};
    use std::cell::RefCell;
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    fn ready<F: Future>(future: F) -> F::Output {
        let mut future = Box::pin(future);
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(result) => result,
            Poll::Pending => panic!("database fixture must complete synchronously"),
        }
    }

    struct SqliteTransaction<'a> {
        transaction: rusqlite::Transaction<'a>,
        events: &'a RefCell<Vec<&'static str>>,
    }

    impl RedemptionTransaction for SqliteTransaction<'_> {
        async fn claim(&self, magic: &MagicLinkRecord, now: u64) -> Result<usize, StoreError> {
            self.events.borrow_mut().push("claim");
            let claim = SingleUseClaim::MagicLink { record: magic, now };
            let mut statement = self
                .transaction
                .prepare(claim.statement())
                .map_err(|_| StoreError)?;
            let rows = statement
                .query_map(
                    params![
                        magic.token_hash,
                        magic.user_id,
                        magic.email,
                        magic.expires_at,
                        now
                    ],
                    |_| Ok(()),
                )
                .map_err(|_| StoreError)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| StoreError)?;
            Ok(rows.len())
        }

        async fn insert_session(&self, session: &SessionRecord) -> Result<(), StoreError> {
            self.events.borrow_mut().push("insert");
            // Observe the successful claim INSIDE this transaction before the
            // trigger aborts insertion; an early-return fake cannot pass here.
            let used: bool = self
                .transaction
                .query_row("SELECT used FROM magic_link_tokens", [], |row| row.get(0))
                .unwrap();
            assert!(used, "session insertion must follow a successful claim");
            self.transaction
                .execute(
                    INSERT_SESSION,
                    params![
                        session.id,
                        session.user_id,
                        session.created_at,
                        session.expires_at,
                        session.rotation_counter,
                    ],
                )
                .map(|_| ())
                .map_err(|_| StoreError)
        }

        async fn commit(self) -> Result<(), StoreError> {
            self.events.borrow_mut().push("commit");
            self.transaction.commit().map_err(|_| StoreError)
        }

        async fn rollback(self) -> Result<(), StoreError> {
            self.events.borrow_mut().push("rollback");
            self.transaction.rollback().map_err(|_| StoreError)
        }
    }

    fn fixture() -> (Connection, MagicLinkRecord, SessionRecord) {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE magic_link_tokens (
                token_hash TEXT PRIMARY KEY, user_id TEXT, email TEXT,
                issued_at INTEGER, expires_at INTEGER, used BOOLEAN);
             CREATE TABLE sessions (
                id TEXT PRIMARY KEY, user_id TEXT, created_at INTEGER,
                expires_at INTEGER, rotation_counter INTEGER, revoked BOOLEAN);
             INSERT INTO magic_link_tokens VALUES ('hash', 'u1', 'a@example.com', 1000, 1900, FALSE);",
        )
        .unwrap();
        let magic = MagicLinkRecord {
            token_hash: "hash".into(),
            user_id: "u1".into(),
            email: "a@example.com".into(),
            issued_at: 1000,
            expires_at: 1900,
            used: true, // The handler passes the prepared record, not storage's unused row.
        };
        let session = SessionRecord {
            id: "session".into(),
            user_id: "u1".into(),
            created_at: 1001,
            expires_at: 4601,
            rotation_counter: 0,
            revoked: false,
        };
        (db, magic, session)
    }

    fn persisted(db: &Connection) -> (bool, usize) {
        let used = db
            .query_row("SELECT used FROM magic_link_tokens", [], |row| row.get(0))
            .unwrap();
        let sessions = db
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .unwrap();
        (used, sessions)
    }

    #[test]
    fn failure_after_claim_rolls_back_and_same_link_can_retry_once() {
        let (mut db, magic, session) = fixture();
        let events = RefCell::new(Vec::new());
        db.execute_batch(
            "CREATE TRIGGER fail_session_insert BEFORE INSERT ON sessions
             BEGIN SELECT RAISE(ABORT, 'injected session insertion failure'); END;",
        )
        .unwrap();
        let failed = ready(redeem(
            SqliteTransaction {
                transaction: db.transaction().unwrap(),
                events: &events,
            },
            &magic,
            1001,
            &session,
        ));
        assert!(failed.is_err());
        assert_eq!(*events.borrow(), ["claim", "insert", "rollback"]);
        assert_eq!(persisted(&db), (false, 0));

        db.execute_batch("DROP TRIGGER fail_session_insert")
            .unwrap();
        events.borrow_mut().clear();
        let retry = ready(redeem(
            SqliteTransaction {
                transaction: db.transaction().unwrap(),
                events: &events,
            },
            &magic,
            1001,
            &session,
        ));
        assert!(matches!(retry, Ok(true)));
        assert_eq!(*events.borrow(), ["claim", "insert", "commit"]);
        assert_eq!(persisted(&db), (true, 1));

        events.borrow_mut().clear();
        let replay = ready(redeem(
            SqliteTransaction {
                transaction: db.transaction().unwrap(),
                events: &events,
            },
            &magic,
            1001,
            &session,
        ));
        assert!(matches!(replay, Ok(false)));
        assert_eq!(*events.borrow(), ["claim", "rollback"]);
        assert_eq!(persisted(&db), (true, 1));
    }

    #[test]
    fn worker_passes_one_owned_transaction_to_the_tested_redemption() {
        let source: String = include_str!("worker_entry.rs")
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<String>()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let method = source
            .split_once("asyncfnredeem_magic_link(")
            .unwrap()
            .1
            .split_once("asyncfnsession_by_id(")
            .unwrap()
            .0;
        assert!(
            method.contains("lettransaction=client.transaction().await.map_err(|_|StoreError)?;")
        );
        assert!(
            method.contains("crate::magic_redemption::redeem(transaction,magic,now,session).await")
        );
        for bypass in [".query(", ".execute(", ".commit(", ".rollback("] {
            assert!(
                !method.contains(bypass),
                "redemption bypasses the shared transaction seam"
            );
        }
    }

    #[test]
    fn mismatched_session_owner_never_claims() {
        let (mut db, magic, mut session) = fixture();
        session.user_id = "other".into();
        let events = RefCell::new(Vec::new());
        let result = ready(redeem(
            SqliteTransaction {
                transaction: db.transaction().unwrap(),
                events: &events,
            },
            &magic,
            1001,
            &session,
        ));
        assert!(result.is_err());
        assert_eq!(*events.borrow(), ["rollback"]);
        assert_eq!(persisted(&db), (false, 0));
    }
}
