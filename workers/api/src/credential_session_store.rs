//! PostgreSQL credential/session writes, shared by Hyperdrive and native tests.

use crate::handlers::{NativeCredentialRow, StoreError};
use tokio_postgres::{types::Type, Client, IsolationLevel};

/// Retain the account-deletion fence before locking the issuing session. Both
/// locks live through commit. A session check without a conflicting row lock
/// would only validate a snapshot, not serialize the write with revocation.
pub async fn insert_native_credential(
    client: &mut Client,
    row: &NativeCredentialRow,
    session_id: &str,
) -> Result<(), StoreError> {
    if row.record.user_id.is_empty() || session_id.is_empty() {
        return Err(StoreError);
    }
    let transaction = client
        .build_transaction()
        .isolation_level(IsolationLevel::ReadCommitted)
        .start()
        .await
        .map_err(|_| StoreError)?;
    let owner = transaction
        .query_typed(
            "SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE",
            &[(&row.record.user_id, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    if owner.len() != 1 {
        return Err(StoreError);
    }
    let session = transaction
        .query_typed(
            "SELECT id FROM sessions WHERE id = $1 AND user_id = $2 AND revoked = FALSE FOR SHARE",
            &[(&session_id, Type::TEXT), (&row.record.user_id, Type::TEXT)],
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
                (
                    &i64::try_from(row.record.created_at).map_err(|_| StoreError)?,
                    Type::INT8,
                ),
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
    client: &mut Client,
    user_id: &str,
    now: u64,
) -> Result<u64, StoreError> {
    if user_id.is_empty() {
        return Ok(0);
    }
    let now = i64::try_from(now).map_err(|_| StoreError)?;
    // Migration 0037 may still be pending in production. The compat table
    // must not turn this existing native path into a 500 before it lands:
    // probe first, and only touch the table when it exists. A probe failure
    // stays loud so a real database error never skips revocation silently.
    let compat_table = client
        .query_typed(
            "SELECT to_regclass('public.subsonic_credentials') IS NOT NULL",
            &[],
        )
        .await
        .map_err(|_| StoreError)?;
    let compat_table = compat_table
        .first()
        .map(|row| row.get::<_, bool>(0))
        .unwrap_or(false);
    let transaction = client
        .build_transaction()
        .isolation_level(IsolationLevel::ReadCommitted)
        .start()
        .await
        .map_err(|_| StoreError)?;
    transaction
        .query_typed(
            "UPDATE sessions SET revoked = TRUE WHERE user_id = $1",
            &[(&user_id, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    let revoked = transaction
        .query_typed(
            "UPDATE native_rest_credentials SET revoked_at = $2 \
             WHERE user_id = $1 AND revoked_at IS NULL RETURNING id",
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
