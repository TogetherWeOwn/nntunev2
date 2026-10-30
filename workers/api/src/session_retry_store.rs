//! PostgreSQL refresh receipt execution, shared by Hyperdrive and native tests.

use crate::handlers::StoreError;
use crate::session::{
    SessionClaims, MAX_SESSION_SECS, REFRESH_RETRY_SECS, ROTATE_SESSION_WITH_RETRY_SQL,
};

fn valid_token_times(claims: &SessionClaims) -> bool {
    !claims.session_id.is_empty()
        && !claims.user_id.is_empty()
        && claims.created_at <= claims.issued_at
        && claims.issued_at < claims.expires_at
        && claims.expires_at <= claims.created_at.saturating_add(MAX_SESSION_SECS)
}

/// Serialize before sampling time. Evaluating a volatile clock in an UPDATE's
/// WHERE clause is not enough: PostgreSQL can evaluate it before a row-lock wait.
/// The lock, clock, parameter binding, row decoding and commit live here so the
/// native PostgreSQL contention tests execute the production adapter itself.
pub async fn rotate_with_retry(
    client: &mut tokio_postgres::Client,
    previous: &SessionClaims,
    next: &SessionClaims,
    retry_hash: &str,
) -> Result<Option<SessionClaims>, StoreError> {
    // A receipt clock may differ from the Worker's clock. Persist the candidate's
    // already-validated token issuance, never the database receipt timestamp.
    if !valid_token_times(next)
        || next.session_id != previous.session_id
        || next.user_id != previous.user_id
        || next.created_at != previous.created_at
        || previous.rotation_counter.checked_add(1) != Some(next.rotation_counter)
    {
        return Err(StoreError);
    }
    let transaction = client.transaction().await.map_err(|_| StoreError)?;
    if transaction
        .query_opt(
            "SELECT id FROM sessions WHERE id = $1 FOR UPDATE",
            &[&previous.session_id],
        )
        .await
        .map_err(|_| StoreError)?
        .is_none()
    {
        return Ok(None);
    }
    // The clock is sampled ONCE, inside the claim statement AFTER the lock.
    // There is no asynchronous client-side gap between a clock query and the
    // UPDATE. now()/statement_timestamp() could retain an older snapshot.
    // Keep the portable predicate used by the supplemental SQLite tests, but
    // substitute its two time inputs with one materialized database clock.
    let claim_sql = format!(
        "WITH claim_clock AS MATERIALIZED (SELECT floor(extract(epoch FROM clock_timestamp()))::bigint AS at) {}",
        ROTATE_SESSION_WITH_RETRY_SQL
            .replace("$11", &format!("(SELECT at - {REFRESH_RETRY_SECS} FROM claim_clock)"))
            .replace("$10", "(SELECT at FROM claim_clock)")
    );
    let row = transaction
        .query_opt(
            &claim_sql,
            &[
                &previous.session_id,
                &previous.user_id,
                &i64::try_from(previous.created_at).map_err(|_| StoreError)?,
                &i32::try_from(previous.rotation_counter).map_err(|_| StoreError)?,
                &i64::try_from(previous.expires_at).map_err(|_| StoreError)?,
                &i32::try_from(next.rotation_counter).map_err(|_| StoreError)?,
                &i64::try_from(next.expires_at).map_err(|_| StoreError)?,
                &retry_hash,
                &i64::try_from(next.issued_at).map_err(|_| StoreError)?,
            ],
        )
        .await
        .map_err(|_| StoreError)?;
    let claims = row
        .map(|row| -> Result<SessionClaims, StoreError> {
            let created_at: i64 = row.try_get("created_at").map_err(|_| StoreError)?;
            let issued_at: i64 = row.try_get("refresh_issued_at").map_err(|_| StoreError)?;
            let expires_at: i64 = row.try_get("expires_at").map_err(|_| StoreError)?;
            let counter: i32 = row.try_get("rotation_counter").map_err(|_| StoreError)?;
            Ok(SessionClaims {
                session_id: row.try_get("id").map_err(|_| StoreError)?,
                user_id: row.try_get("user_id").map_err(|_| StoreError)?,
                created_at: u64::try_from(created_at).map_err(|_| StoreError)?,
                issued_at: u64::try_from(issued_at).map_err(|_| StoreError)?,
                expires_at: u64::try_from(expires_at).map_err(|_| StoreError)?,
                rotation_counter: u32::try_from(counter).map_err(|_| StoreError)?,
            })
        })
        .transpose()?;
    if let Some(ref issued) = claims {
        // Decode and validate BEFORE commit: even a broken adapter must not
        // strand the predecessor by committing an unusable successor token.
        if !valid_token_times(issued)
            || issued.session_id != previous.session_id
            || issued.user_id != previous.user_id
            || issued.created_at != previous.created_at
            || issued.rotation_counter != next.rotation_counter
        {
            return Err(StoreError);
        }
    }
    transaction.commit().await.map_err(|_| StoreError)?;
    Ok(claims)
}
