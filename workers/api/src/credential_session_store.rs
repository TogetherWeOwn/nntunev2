//! PostgreSQL credential/session writes, shared by Hyperdrive and native tests.

use crate::account_deletion::WRITER_FENCE_USER_SQL;
use crate::handlers::{NativeCredentialRow, StoreError};
use crate::postgres::{PgConnect, PgQuery};
use tokio_postgres::types::Type;

const ISSUING_SESSION_SQL: &str =
    "SELECT id FROM sessions WHERE id = $1 AND user_id = $2 AND revoked = FALSE \
     AND created_at <= $3 AND expires_at > $3 FOR SHARE";
const REVOKE_SESSIONS_SQL: &str = "UPDATE sessions SET revoked = TRUE WHERE user_id = $1";
const REVOKE_NATIVE_CREDENTIALS_SQL: &str = "UPDATE native_rest_credentials SET revoked_at = $2 \
     WHERE user_id = $1 AND revoked_at IS NULL RETURNING id";

/// Retain the account-deletion fence before locking the issuing session. Both
/// locks live through commit. A session check without a conflicting row lock
/// would only validate a snapshot, not serialize the write with revocation.
/// The locked session must be live at the server's credential issuance time.
pub async fn insert_native_credential(
    client: &mut impl PgConnect,
    row: &NativeCredentialRow,
    session_id: &str,
) -> Result<(), StoreError> {
    if row.record.user_id.is_empty() || session_id.is_empty() {
        return Err(StoreError);
    }
    let issued_at = i64::try_from(row.record.created_at).map_err(|_| StoreError)?;
    let transaction = client.read_committed().await.map_err(|_| StoreError)?;
    let owner = transaction
        .query_typed(WRITER_FENCE_USER_SQL, &[(&row.record.user_id, Type::TEXT)])
        .await
        .map_err(|_| StoreError)?;
    if owner.len() != 1 {
        return Err(StoreError);
    }
    let session = transaction
        .query_typed(
            ISSUING_SESSION_SQL,
            &[
                (&session_id, Type::TEXT),
                (&row.record.user_id, Type::TEXT),
                (&issued_at, Type::INT8),
            ],
        )
        .await
        .map_err(|_| StoreError)?;
    if session.len() != 1 {
        return Err(StoreError);
    }
    let scopes: Vec<String> = row
        .record
        .scopes
        .iter()
        .map(|scope| scope.as_str().to_string())
        .collect();
    let inserted = transaction
        .query_typed(
            "INSERT INTO native_rest_credentials (id, user_id, label, key_prefix, key_hmac, \
             pepper_version, scopes, created_at, expires_at, created_from_ip) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
            &[
                (&row.record.id, Type::TEXT),
                (&row.record.user_id, Type::TEXT),
                (&row.label, Type::TEXT),
                (&row.record.key_prefix, Type::TEXT),
                (&row.record.key_hmac, Type::TEXT),
                (
                    &i32::try_from(row.record.pepper_version).map_err(|_| StoreError)?,
                    Type::INT4,
                ),
                (&scopes, Type::TEXT_ARRAY),
                (&issued_at, Type::INT8),
                (
                    &i64::try_from(row.record.expires_at).map_err(|_| StoreError)?,
                    Type::INT8,
                ),
                (&row.created_from_ip, Type::TEXT),
            ],
        )
        .await
        .map_err(|_| StoreError)?;
    if inserted.len() != 1 {
        return Err(StoreError);
    }
    transaction.commit().await.map_err(|_| StoreError)
}

/// Revoke sessions before native and compatibility credentials in one
/// transaction. Return only the newly revoked native count. The session UPDATE
/// conflicts with issuance's FOR SHARE lock. Under READ COMMITTED the following
/// credential UPDATE sees any issuance that committed before that lock released.
/// Revocation never acquires a user lock after the session lock, preserving the
/// user -> session -> credential lock order of issuance and account deletion.
pub async fn revoke_sessions_and_native_credentials(
    client: &mut impl PgConnect,
    user_id: &str,
    now: u64,
) -> Result<u64, StoreError> {
    if user_id.is_empty() {
        return Ok(0);
    }
    let now = i64::try_from(now).map_err(|_| StoreError)?;
    let transaction = client.read_committed().await.map_err(|_| StoreError)?;
    // Migration 0037 may still be pending in production. The compat table
    // must not turn this existing native path into a 500 before it lands:
    // probe first, and only touch the table when it exists. A probe failure
    // stays loud so a real database error never skips revocation silently.
    // Unqualified like the UPDATE below: the probe must resolve the same
    // table the compat update reaches on the same transaction backend.
    let compat_table: bool = transaction
        .query_typed_one(
            "SELECT to_regclass('subsonic_credentials') IS NOT NULL",
            &[],
        )
        .await
        .map_err(|_| StoreError)?
        .try_get(0)
        .map_err(|_| StoreError)?;
    transaction
        .query_typed(REVOKE_SESSIONS_SQL, &[(&user_id, Type::TEXT)])
        .await
        .map_err(|_| StoreError)?;
    let revoked = transaction
        .query_typed(
            REVOKE_NATIVE_CREDENTIALS_SQL,
            &[(&user_id, Type::TEXT), (&now, Type::INT8)],
        )
        .await
        .map_err(|_| StoreError)?;
    if compat_table {
        transaction
            .query_typed(
                "UPDATE subsonic_credentials SET revoked_at = to_timestamp($2::bigint::double precision) \
                 WHERE user_id = $1 AND revoked_at IS NULL",
                &[(&user_id, Type::TEXT), (&now, Type::INT8)],
            )
            .await
            .map_err(|_| StoreError)?;
    }
    transaction.commit().await.map_err(|_| StoreError)?;
    Ok(revoked.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{named_params, Connection};

    #[test]
    fn issuing_session_lookup_requires_owner_lifetime_and_no_revocation() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, user_id TEXT, revoked BOOL,
                                    created_at INTEGER, expires_at INTEGER);
             INSERT INTO sessions VALUES
               ('active', 'owner', FALSE, 10, 20),
               ('revoked', 'owner', TRUE, 10, 20),
               ('foreign', 'other', FALSE, 10, 20);",
        )
        .unwrap();
        let mut lookup = db
            .prepare(ISSUING_SESSION_SQL.strip_suffix(" FOR SHARE").unwrap())
            .unwrap();
        for (id, owner, now, expected) in [
            ("active", "owner", 10, true),
            ("active", "owner", 19, true),
            ("active", "owner", 20, false),
            ("active", "owner", 9, false),
            ("revoked", "owner", 10, false),
            ("foreign", "owner", 10, false),
            ("missing", "owner", 10, false),
        ] {
            assert_eq!(
                lookup
                    .exists(named_params! { "$1": id, "$2": owner, "$3": now })
                    .unwrap(),
                expected,
                "{id} / {owner} / {now}"
            );
        }
    }

    #[test]
    fn revocation_is_owner_scoped_and_preserves_first_timestamp() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE sessions (user_id TEXT, revoked BOOL);
             INSERT INTO sessions VALUES ('owner', FALSE), ('other', FALSE);
             CREATE TABLE native_rest_credentials (id TEXT, user_id TEXT, revoked_at INTEGER);
             INSERT INTO native_rest_credentials VALUES
                ('active', 'owner', NULL), ('revoked', 'owner', 4), ('foreign', 'other', NULL);",
        )
        .unwrap();
        db.execute(REVOKE_SESSIONS_SQL, ["owner"]).unwrap();
        let mut revoke = db.prepare(REVOKE_NATIVE_CREDENTIALS_SQL).unwrap();
        for (now, expected) in [(5, vec!["active".to_owned()]), (6, vec![])] {
            let revoked = revoke
                .query_map(named_params! { "$1": "owner", "$2": now }, |row| {
                    row.get::<_, String>(0)
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(revoked, expected);
        }
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM sessions WHERE revoked AND user_id = 'owner'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM sessions WHERE revoked AND user_id = 'other'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.prepare("SELECT revoked_at FROM native_rest_credentials ORDER BY id")
                .unwrap()
                .query_map([], |row| row.get::<_, Option<i64>>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            [Some(5), None, Some(4)]
        );
    }
}
