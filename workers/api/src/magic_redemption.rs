//! One transaction owns the magic-link claim, session insert and commit.
//! Kept independent of the Worker runtime so failure-after-claim tests execute
//! the same orchestration against a real, injectable database transaction.
//!
//! Expiry is evaluated by the database when the claim executes, never from a
//! request-admission timestamp: the claim statement carries the backend's own
//! clock expression in its single dialect slot. The same slot carries the
//! user-row lock, taken before the token row lock so redemption and the
//! `register_start` reaper (user row, then its cascading token rows) acquire
//! locks in one consistent order. Parameter values and their bind order come
//! from the shared builders below, so the PostgreSQL and fixture bindings
//! cannot drift apart.

use crate::auth::MagicLinkRecord;
use crate::handlers::StoreError;
use crate::session::SessionRecord;
use crate::vault::AuditEvent;

pub(crate) trait RedemptionTransaction: Sized {
    /// Bound statement execution on the database side, inside this
    /// transaction, so a stalled claim or insert cannot hold the token row
    /// past the response deadline after the client future is dropped.
    async fn apply_statement_timeout(&self) -> Result<(), StoreError>;
    async fn claim(&self, magic: &MagicLinkRecord) -> Result<usize, StoreError>;
    async fn insert_session(&self, session: &SessionRecord) -> Result<(), StoreError>;
    async fn insert_audit(&self, event: &AuditEvent) -> Result<(), StoreError>;
    async fn commit(self) -> Result<(), StoreError>;
    async fn rollback(self) -> Result<(), StoreError>;
}

/// One ordered bind value. The builders below are the single source of
/// parameter order and integer conversion for every backend; each backend
/// only maps this representation onto its own driver types.
#[derive(Debug, PartialEq, Eq)]
enum Param<'a> {
    Text(&'a str),
    Int64(i64),
    Int32(i32),
    Bool(bool),
}

fn claim_params(magic: &MagicLinkRecord) -> Result<[Param<'_>; 4], StoreError> {
    Ok([
        Param::Text(&magic.token_hash),
        Param::Text(&magic.user_id),
        Param::Text(&magic.email),
        Param::Int64(i64::try_from(magic.expires_at).map_err(|_| StoreError)?),
    ])
}

fn session_params(session: &SessionRecord) -> Result<[Param<'_>; 5], StoreError> {
    Ok([
        Param::Text(&session.id),
        Param::Text(&session.user_id),
        Param::Int64(i64::try_from(session.created_at).map_err(|_| StoreError)?),
        Param::Int64(i64::try_from(session.expires_at).map_err(|_| StoreError)?),
        Param::Int32(i32::try_from(session.rotation_counter).map_err(|_| StoreError)?),
    ])
}

fn audit_params(event: &AuditEvent) -> Result<[Param<'_>; 5], StoreError> {
    Ok([
        Param::Text(&event.user_id),
        Param::Text(&event.action),
        Param::Bool(event.ok),
        Param::Text(&event.source_ip),
        Param::Int64(i64::try_from(event.at).map_err(|_| StoreError)?),
    ])
}

/// The conditional single-use claim as one template. `db_now` is the
/// backend's own current-time expression (seconds), evaluated when the claim
/// executes; `user_row_lock` is the backend's user-row locking clause (empty
/// where the backend has no row locks). The user existence predicate doubles
/// as the lock-order anchor: the user row is locked by the uncorrelated
/// subquery before the token row is updated, matching the reaper's
/// user-then-token order. Outside those two dialect slots the predicates and
/// parameter positions are exactly the canonical
/// [`crate::auth::SingleUseClaim`] magic-link statement (pinned by test).
fn claim_statement(db_now: &str, user_row_lock: &str) -> String {
    format!(
        "UPDATE magic_link_tokens SET used = TRUE \
         WHERE token_hash = $1 AND user_id = $2 AND email = $3 AND expires_at = $4 \
         AND expires_at > {db_now} AND used = FALSE \
         AND user_id = (SELECT id FROM users WHERE id = $2{user_row_lock}) \
         RETURNING token_hash"
    )
}

const INSERT_SESSION: &str =
    "INSERT INTO sessions (id, user_id, created_at, expires_at, rotation_counter, revoked) \
     VALUES ($1, $2, $3, $4, $5, FALSE)";

const INSERT_AUDIT: &str =
    "INSERT INTO audit_log (user_id, action, ok, source_ip, at) VALUES ($1, $2, $3, $4, $5)";

const PG_DB_NOW: &str = "FLOOR(EXTRACT(EPOCH FROM clock_timestamp()))::BIGINT";
const PG_USER_ROW_LOCK: &str = " FOR KEY SHARE";

fn pg_params<'a>(params: &'a [Param<'a>]) -> Vec<&'a (dyn tokio_postgres::types::ToSql + Sync)> {
    params
        .iter()
        .map(|param| match param {
            Param::Text(value) => *value as &(dyn tokio_postgres::types::ToSql + Sync),
            Param::Int64(value) => value as &(dyn tokio_postgres::types::ToSql + Sync),
            Param::Int32(value) => value as &(dyn tokio_postgres::types::ToSql + Sync),
            Param::Bool(value) => value as &(dyn tokio_postgres::types::ToSql + Sync),
        })
        .collect()
}

impl RedemptionTransaction for tokio_postgres::Transaction<'_> {
    async fn apply_statement_timeout(&self) -> Result<(), StoreError> {
        // `SET LOCAL` is scoped to this transaction, so it lands on the one
        // backend the transaction pins — unlike the connect-time `SET`,
        // which transaction-mode pooling may apply to a different backend.
        self.batch_execute(&format!(
            "SET LOCAL statement_timeout = {}",
            crate::vault::TOTAL_TIMEOUT_MS
        ))
        .await
        .map_err(|_| StoreError)
    }

    async fn claim(&self, magic: &MagicLinkRecord) -> Result<usize, StoreError> {
        let statement = claim_statement(PG_DB_NOW, PG_USER_ROW_LOCK);
        let params = claim_params(magic)?;
        let params = pg_params(&params);
        self.query(statement.as_str(), params.as_slice())
            .await
            .map(|rows| rows.len())
            .map_err(|_| StoreError)
    }

    async fn insert_session(&self, session: &SessionRecord) -> Result<(), StoreError> {
        let params = session_params(session)?;
        let params = pg_params(&params);
        self.execute(INSERT_SESSION, params.as_slice())
            .await
            .map(|_| ())
            .map_err(|_| StoreError)
    }

    async fn insert_audit(&self, event: &AuditEvent) -> Result<(), StoreError> {
        let params = audit_params(event)?;
        let params = pg_params(&params);
        self.execute(INSERT_AUDIT, params.as_slice())
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
    session: &SessionRecord,
    audit: Option<&AuditEvent>,
) -> Result<bool, StoreError> {
    if session.user_id != magic.user_id {
        transaction.rollback().await?;
        return Err(StoreError);
    }
    if transaction.apply_statement_timeout().await.is_err() {
        transaction.rollback().await?;
        return Err(StoreError);
    }
    match transaction.claim(magic).await {
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
    // The success audit commits with the claim and the session, so no store
    // operation remains between this commit and the cookie response; an
    // audit failure rolls everything back and leaves the link retryable.
    if let Some(event) = audit {
        if transaction.insert_audit(event).await.is_err() {
            transaction.rollback().await?;
            return Err(StoreError);
        }
    }
    // Drop also rolls back on cancellation. Never reset a spent token with a
    // compensating write, nor reopen it after an ambiguous COMMIT response.
    transaction.commit().await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::SingleUseClaim;
    use rusqlite::{params_from_iter, Connection};
    use std::cell::RefCell;
    use std::future::Future;
    use std::task::{Context, Poll, Waker};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// SQLite's own clock, the fixture's counterpart of `PG_DB_NOW`: expiry
    /// is evaluated when the claim executes, so fixture rows use real
    /// wall-clock-relative timestamps below.
    const SQLITE_DB_NOW: &str = "CAST(strftime('%s', 'now') AS INTEGER)";

    fn sqlite_values(params: &[Param<'_>]) -> Vec<rusqlite::types::Value> {
        params
            .iter()
            .map(|param| match param {
                Param::Text(value) => rusqlite::types::Value::Text((*value).to_string()),
                Param::Int64(value) => rusqlite::types::Value::Integer(*value),
                Param::Int32(value) => rusqlite::types::Value::Integer(i64::from(*value)),
                Param::Bool(value) => rusqlite::types::Value::Integer(i64::from(*value)),
            })
            .collect()
    }

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
        async fn apply_statement_timeout(&self) -> Result<(), StoreError> {
            self.events.borrow_mut().push("bound");
            // SQLite has no per-statement server timeout; its busy timeout is
            // the fixture's bound on lock waits, applied before any write.
            self.transaction
                .execute_batch(&format!(
                    "PRAGMA busy_timeout = {}",
                    crate::vault::TOTAL_TIMEOUT_MS
                ))
                .map_err(|_| StoreError)
        }

        async fn claim(&self, magic: &MagicLinkRecord) -> Result<usize, StoreError> {
            self.events.borrow_mut().push("claim");
            // No row-lock clause: SQLite has no `FOR KEY SHARE`; the user
            // existence predicate and the database clock still execute.
            let statement = claim_statement(SQLITE_DB_NOW, "");
            let params = claim_params(magic)?;
            let mut statement = self
                .transaction
                .prepare(&statement)
                .map_err(|_| StoreError)?;
            let rows = statement
                .query_map(params_from_iter(sqlite_values(&params)), |_| Ok(()))
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
            let params = session_params(session)?;
            self.transaction
                .execute(INSERT_SESSION, params_from_iter(sqlite_values(&params)))
                .map(|_| ())
                .map_err(|_| StoreError)
        }

        async fn insert_audit(&self, event: &AuditEvent) -> Result<(), StoreError> {
            self.events.borrow_mut().push("audit");
            let params = audit_params(event)?;
            self.transaction
                .execute(INSERT_AUDIT, params_from_iter(sqlite_values(&params)))
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
        // The claim reads the database clock, so fixture timestamps are
        // relative to the real wall clock, not a captured request time.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(&format!(
            "CREATE TABLE users (id TEXT PRIMARY KEY);
             CREATE TABLE magic_link_tokens (
                token_hash TEXT PRIMARY KEY, user_id TEXT, email TEXT,
                issued_at INTEGER, expires_at INTEGER, used BOOLEAN);
             CREATE TABLE sessions (
                id TEXT PRIMARY KEY, user_id TEXT, created_at INTEGER,
                expires_at INTEGER, rotation_counter INTEGER, revoked BOOLEAN);
             CREATE TABLE audit_log (
                user_id TEXT, action TEXT, ok BOOLEAN, source_ip TEXT, at INTEGER);
             INSERT INTO users VALUES ('u1');
             INSERT INTO magic_link_tokens VALUES ('hash', 'u1', 'a@example.com', {now}, {}, FALSE);",
            now + 900
        ))
        .unwrap();
        let magic = MagicLinkRecord {
            token_hash: "hash".into(),
            user_id: "u1".into(),
            email: "a@example.com".into(),
            issued_at: now,
            expires_at: now + 900,
            used: true, // The handler passes the prepared record, not storage's unused row.
        };
        let session = SessionRecord {
            id: "session".into(),
            user_id: "u1".into(),
            created_at: now + 1,
            expires_at: now + 3601,
            rotation_counter: 0,
            revoked: false,
        };
        (db, magic, session)
    }

    fn success_audit(magic: &MagicLinkRecord) -> AuditEvent {
        crate::vault::audit_event(
            &magic.user_id,
            "auth.magic",
            true,
            "203.0.113.9",
            magic.issued_at,
        )
    }

    fn persisted(db: &Connection) -> (bool, usize, usize) {
        let used = db
            .query_row("SELECT used FROM magic_link_tokens", [], |row| row.get(0))
            .unwrap();
        let sessions: i64 = db
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .unwrap();
        let audits: i64 = db
            .query_row("SELECT COUNT(*) FROM audit_log", [], |row| row.get(0))
            .unwrap();
        (
            used,
            usize::try_from(sessions).unwrap(),
            usize::try_from(audits).unwrap(),
        )
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
            &session,
            Some(&success_audit(&magic)),
        ));
        assert!(failed.is_err());
        assert_eq!(*events.borrow(), ["bound", "claim", "insert", "rollback"]);
        assert_eq!(persisted(&db), (false, 0, 0));

        db.execute_batch("DROP TRIGGER fail_session_insert")
            .unwrap();
        events.borrow_mut().clear();
        let retry = ready(redeem(
            SqliteTransaction {
                transaction: db.transaction().unwrap(),
                events: &events,
            },
            &magic,
            &session,
            Some(&success_audit(&magic)),
        ));
        assert!(matches!(retry, Ok(true)));
        assert_eq!(
            *events.borrow(),
            ["bound", "claim", "insert", "audit", "commit"]
        );
        assert_eq!(persisted(&db), (true, 1, 1));

        events.borrow_mut().clear();
        let replay = ready(redeem(
            SqliteTransaction {
                transaction: db.transaction().unwrap(),
                events: &events,
            },
            &magic,
            &session,
            Some(&success_audit(&magic)),
        ));
        assert!(matches!(replay, Ok(false)));
        assert_eq!(*events.borrow(), ["bound", "claim", "rollback"]);
        assert_eq!(persisted(&db), (true, 1, 1));
    }

    #[test]
    fn claim_evaluates_expiry_against_the_database_clock() {
        // The seam takes no admission timestamp at all: a link that has
        // expired by the database's own clock at claim time loses, even
        // though its snapshot was issued while valid.
        let (mut db, mut magic, session) = fixture();
        let expired_at = magic.issued_at - 1;
        db.execute(
            "UPDATE magic_link_tokens SET expires_at = ?1",
            [i64::try_from(expired_at).unwrap()],
        )
        .unwrap();
        magic.expires_at = expired_at;
        let events = RefCell::new(Vec::new());
        let result = ready(redeem(
            SqliteTransaction {
                transaction: db.transaction().unwrap(),
                events: &events,
            },
            &magic,
            &session,
            Some(&success_audit(&magic)),
        ));
        assert!(matches!(result, Ok(false)));
        assert_eq!(*events.borrow(), ["bound", "claim", "rollback"]);
        assert_eq!(persisted(&db), (false, 0, 0));
    }

    #[test]
    fn shared_parameter_builders_pin_order_and_conversions() {
        let (_, magic, session) = fixture();
        assert_eq!(
            claim_params(&magic).unwrap(),
            [
                Param::Text("hash"),
                Param::Text("u1"),
                Param::Text("a@example.com"),
                Param::Int64(i64::try_from(magic.expires_at).unwrap()),
            ]
        );
        assert_eq!(
            session_params(&session).unwrap(),
            [
                Param::Text("session"),
                Param::Text("u1"),
                Param::Int64(i64::try_from(session.created_at).unwrap()),
                Param::Int64(i64::try_from(session.expires_at).unwrap()),
                Param::Int32(0),
            ]
        );
        assert_eq!(
            audit_params(&success_audit(&magic)).unwrap(),
            [
                Param::Text("u1"),
                Param::Text("auth.magic"),
                Param::Bool(true),
                Param::Text("203.0.113.9"),
                Param::Int64(i64::try_from(magic.issued_at).unwrap()),
            ]
        );
        // Out-of-range values fail closed in the builder, before any backend
        // can bind a truncated integer.
        let mut wide = magic.clone();
        wide.expires_at = u64::MAX;
        assert!(claim_params(&wide).is_err());
        let mut wide = session.clone();
        wide.rotation_counter = u32::MAX;
        assert!(session_params(&wide).is_err());
    }

    #[test]
    fn claim_template_matches_the_single_use_statement_outside_its_dialect_slots() {
        let (_, magic, _) = fixture();
        let canonical = SingleUseClaim::MagicLink {
            record: &magic,
            now: 0,
        }
        .statement();
        let normalized = claim_statement("$5", "")
            .replace(" AND user_id = (SELECT id FROM users WHERE id = $2)", "");
        assert_eq!(normalized, canonical);
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
        let methods = source
            .split_once("asyncfnredeem_magic_link(")
            .unwrap()
            .1
            .split_once("asyncfnsession_by_id(")
            .unwrap()
            .0;
        // Each production redemption entry point hands its one owned
        // transaction to the seam the SQLite fixture executes; neither may
        // run statements around it.
        let (plain, audited) = methods
            .split_once("asyncfnredeem_magic_link_and_audit(")
            .unwrap();
        for (method, call) in [
            (
                plain,
                "crate::magic_redemption::redeem(transaction,magic,session,None).await",
            ),
            (
                audited,
                "crate::magic_redemption::redeem(transaction,magic,session,Some(audit)).await",
            ),
        ] {
            assert!(method
                .contains("lettransaction=client.transaction().await.map_err(|_|StoreError)?;"));
            assert!(method.contains(call));
            for bypass in [".query(", ".execute(", ".commit(", ".rollback("] {
                assert!(
                    !method.contains(bypass),
                    "redemption bypasses the shared transaction seam"
                );
            }
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
            &session,
            Some(&success_audit(&magic)),
        ));
        assert!(result.is_err());
        assert_eq!(*events.borrow(), ["rollback"]);
        assert_eq!(persisted(&db), (false, 0, 0));
    }
}
