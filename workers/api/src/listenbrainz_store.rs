//! Durable ListenBrainz credential lifecycle. No HTTP or provider calls.
//!
//! Callers authenticate/freshness-check the session and validate the token's
//! upstream username before replacing a link. Both mutations lock the owner
//! row (including when no link exists), reject the account-deletion fence,
//! and commit the credential change and scalar-only audit together. Relinking
//! deletes then inserts; there is no second credential copy or UPSERT.

use tokio_postgres::{types::Type, Client, GenericClient, Row};

use crate::{
    account_deletion::FencedWriteOutcome,
    field_envelope::FieldEnvelope,
    handlers::StoreError,
    listenbrainz::{OutboundClaim, RewrapOutcome, StoredLink, Username},
    listenbrainz_rotation::{check_scan_args, OwnerCursor, MAX_SCAN_LIMIT},
};

// Durable rotation-run ledger transactions; source only until a runtime
// caller is enabled.
#[allow(dead_code)]
pub(crate) mod rotation_runs;

/// Why a transaction-capable helper failed. `Sql` means a statement errored
/// (inside a transaction the whole transaction is then aborted and a caller
/// must not continue); `Data` means a row or argument was rejected locally
/// with the transaction still usable. Neither carries detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepError {
    Sql,
    Data,
}

impl From<StepError> for StoreError {
    fn from(_: StepError) -> Self {
        StoreError
    }
}

// Initialization grants nothing and never resets an existing cooldown. Keep
// it separate so absent-row uniqueness waits finish before sampling the clock.
async fn initialize_outbound(client: &Client) -> Result<(), StoreError> {
    client
        .query_typed(
            "INSERT INTO listenbrainz_outbound_rate (singleton, next_allowed_at_ms) \
             VALUES (TRUE, 0) ON CONFLICT (singleton) DO NOTHING",
            &[],
        )
        .await
        .map_err(|_| StoreError)?;
    Ok(())
}

/// Atomically spend the fleet-wide slot AND reserve its full response cycle.
/// Materialized dependencies sample the clock after locking. A busy claim has
/// no automatic expiry: even a delayed sender excludes later requests until
/// cooldown persistence succeeds. Pooled connections need no session affinity.
pub(crate) async fn claim_outbound(client: &Client) -> Result<Option<OutboundClaim>, StoreError> {
    initialize_outbound(client).await?;
    let interval_ms = i64::try_from(crate::listenbrainz::MIN_REQUEST_INTERVAL_S * 1000)
        .map_err(|_| StoreError)?;
    let rows = client
        .query_typed(
            "WITH locked AS MATERIALIZED \
             (SELECT singleton, next_allowed_at_ms, in_flight, generation \
              FROM listenbrainz_outbound_rate WHERE singleton = TRUE FOR UPDATE), \
             clock AS MATERIALIZED \
             (SELECT *, ceil(extract(epoch FROM clock_timestamp()) * 1000)::bigint AS ms \
              FROM locked) \
             UPDATE listenbrainz_outbound_rate AS budget SET \
             next_allowed_at_ms = clock.ms + $1, in_flight = TRUE, generation = clock.generation + 1 \
             FROM clock WHERE budget.singleton = clock.singleton \
             AND NOT clock.in_flight AND clock.generation < 9223372036854775807 \
             AND clock.next_allowed_at_ms < clock.ms RETURNING budget.generation",
            &[(&interval_ms, Type::INT8)],
        )
        .await
        .map_err(|_| StoreError)?;
    match rows.as_slice() {
        [] => Ok(None),
        [row] => Ok(Some(OutboundClaim(row.try_get(0).map_err(|_| StoreError)?))),
        _ => Err(StoreError),
    }
}

/// Only the matching busy generation may atomically persist the cooldown and
/// release the sender. SQL failure rolls back BOTH changes; a timeout with an
/// unknown commit result never authorizes a retry or success. No initialization
/// here: an absent row is a lost claim, not a fresh budget. Saturate to deny.
pub(crate) async fn finish_outbound(
    client: &Client,
    claim: OutboundClaim,
    wait_s: u64,
) -> Result<(), StoreError> {
    let wait_ms = wait_s.saturating_mul(1000).min(i64::MAX as u64) as i64;
    let rows = client
        .query_typed(
            "WITH locked AS MATERIALIZED \
             (SELECT singleton, next_allowed_at_ms, in_flight, generation \
              FROM listenbrainz_outbound_rate WHERE singleton = TRUE FOR UPDATE), \
             clock AS MATERIALIZED \
             (SELECT *, ceil(extract(epoch FROM clock_timestamp()) * 1000) AS ms FROM locked) \
             UPDATE listenbrainz_outbound_rate AS budget SET next_allowed_at_ms = \
             GREATEST(clock.next_allowed_at_ms, \
              LEAST(clock.ms + $1::numeric, 9223372036854775807)::bigint), in_flight = FALSE \
             FROM clock WHERE budget.singleton = clock.singleton \
             AND clock.in_flight AND clock.generation = $2 RETURNING budget.singleton",
            &[(&wait_ms, Type::INT8), (&claim.0, Type::INT8)],
        )
        .await
        .map_err(|_| StoreError)?;
    if rows.len() != 1 {
        return Err(StoreError);
    }
    Ok(())
}

/// Persist a relative provider cooldown using the database's clock, never
/// shorten a stored floor even for an older response. Saturate unrepresentable
/// waits at BIGINT's maximum (deny, not wrap or a shorter fallback). Callers
/// must persist the parsed delay before returning any upstream outcome; errors
/// must fail closed. This does not implement transport or response ordering.
pub(crate) async fn defer_outbound(client: &Client, wait_s: u64) -> Result<(), StoreError> {
    initialize_outbound(client).await?;
    let wait_ms = wait_s.saturating_mul(1000).min(i64::MAX as u64) as i64;
    let rows = client
        .query_typed(
            "WITH locked AS MATERIALIZED \
             (SELECT singleton, next_allowed_at_ms FROM listenbrainz_outbound_rate \
              WHERE singleton = TRUE FOR UPDATE), \
             clock AS MATERIALIZED \
             (SELECT singleton, next_allowed_at_ms, \
              ceil(extract(epoch FROM clock_timestamp()) * 1000) AS ms FROM locked) \
             UPDATE listenbrainz_outbound_rate AS budget SET next_allowed_at_ms = \
             GREATEST(clock.next_allowed_at_ms, \
              LEAST(clock.ms + $1::numeric, 9223372036854775807)::bigint) \
             FROM clock WHERE budget.singleton = clock.singleton RETURNING budget.singleton",
            &[(&wait_ms, Type::INT8)],
        )
        .await
        .map_err(|_| StoreError)?;
    if rows.len() != 1 {
        return Err(StoreError);
    }
    Ok(())
}

pub(crate) async fn read(client: &Client, user_id: &str) -> Result<Option<StoredLink>, StoreError> {
    Ok(read_in(client, user_id).await?)
}

/// [`read`] on any client or open transaction.
pub(crate) async fn read_in(
    client: &impl GenericClient,
    user_id: &str,
) -> Result<Option<StoredLink>, StepError> {
    let rows = client
        .query_typed(
            "SELECT user_id, username, kek_version, dek_wrapped_nonce, dek_wrapped, \
             payload_nonce, ciphertext FROM listenbrainz_links WHERE user_id = $1",
            &[(&user_id, Type::TEXT)],
        )
        .await
        .map_err(|_| StepError::Sql)?;
    if rows.len() > 1 {
        return Err(StepError::Data);
    }
    rows.first()
        .map(|row| decode(row).map_err(|_| StepError::Data))
        .transpose()
}

fn decode(row: &Row) -> Result<StoredLink, StoreError> {
    let version: i32 = row.try_get("kek_version").map_err(|_| StoreError)?;
    let username: String = row.try_get("username").map_err(|_| StoreError)?;
    let nonce: Vec<u8> = row.try_get("dek_wrapped_nonce").map_err(|_| StoreError)?;
    let payload_nonce: Vec<u8> = row.try_get("payload_nonce").map_err(|_| StoreError)?;
    let link = StoredLink {
        user_id: row.try_get("user_id").map_err(|_| StoreError)?,
        username: Username::parse(&username).map_err(|_| StoreError)?,
        kek_version: u32::try_from(version).map_err(|_| StoreError)?,
        envelope: FieldEnvelope {
            dek_wrapped_nonce: nonce.try_into().map_err(|_| StoreError)?,
            dek_wrapped: row.try_get("dek_wrapped").map_err(|_| StoreError)?,
            payload_nonce: payload_nonce.try_into().map_err(|_| StoreError)?,
            ciphertext: row.try_get("ciphertext").map_err(|_| StoreError)?,
        },
    };
    check_shape(&link.user_id, &link)?;
    Ok(link)
}

fn check_shape(user_id: &str, link: &StoredLink) -> Result<(), StoreError> {
    if link.user_id != user_id
        || link.kek_version == 0
        || i32::try_from(link.kek_version).is_err()
        || link.envelope.dek_wrapped.len() != 48
        || link.envelope.ciphertext.len() != 52
    {
        return Err(StoreError);
    }
    Ok(())
}

/// Keyset page of owner ids whose wrap is older than `below_version`.
/// Selects only `user_id` (no envelope, username or key material), orders by
/// ASCII byte order of the opaque owner id (`COLLATE "C"`) and uses a strict
/// `>` cursor, never OFFSET. Arguments are rejected before the query. A row
/// whose id is not a valid opaque owner id, or an oversize/unordered result, fails
/// the whole scan closed rather than being skipped.
pub(crate) async fn rotation_candidates(
    client: &Client,
    below_version: u32,
    after: Option<&OwnerCursor>,
    limit: u32,
) -> Result<Vec<OwnerCursor>, StoreError> {
    Ok(rotation_candidates_in(client, below_version, after, limit).await?)
}

/// [`rotation_candidates`] on any client or open transaction.
pub(crate) async fn rotation_candidates_in(
    client: &impl GenericClient,
    below_version: u32,
    after: Option<&OwnerCursor>,
    limit: u32,
) -> Result<Vec<OwnerCursor>, StepError> {
    check_scan_args(below_version, limit).map_err(|_| StepError::Data)?;
    let below = i32::try_from(below_version).map_err(|_| StepError::Data)?;
    let cursor: Option<&str> = after.map(OwnerCursor::as_str);
    let rows = client
        .query_typed(
            "SELECT user_id FROM listenbrainz_links WHERE kek_version < $1 \
             AND ($2::text IS NULL OR user_id COLLATE \"C\" > $2::text COLLATE \"C\") \
             ORDER BY user_id COLLATE \"C\" LIMIT $3",
            &[
                (&below, Type::INT4),
                (&cursor, Type::TEXT),
                (&i64::from(limit), Type::INT8),
            ],
        )
        .await
        .map_err(|_| StepError::Sql)?;
    if rows.len() > limit.min(MAX_SCAN_LIMIT) as usize {
        return Err(StepError::Data);
    }
    let mut out: Vec<OwnerCursor> = Vec::with_capacity(rows.len());
    for row in &rows {
        let id: String = row.try_get(0).map_err(|_| StepError::Data)?;
        let id = OwnerCursor::parse(&id).ok_or(StepError::Data)?;
        let previous = out.last().map(OwnerCursor::as_str).or(cursor);
        if previous.is_some_and(|p| id.as_str() <= p) {
            return Err(StepError::Data);
        }
        out.push(id);
    }
    Ok(out)
}

pub(crate) async fn replace(
    client: &mut Client,
    user_id: &str,
    link: &StoredLink,
    source_ip: &str,
    now: u64,
) -> Result<FencedWriteOutcome, StoreError> {
    check_shape(user_id, link)?;
    mutate(client, user_id, Some(link), source_ip, now).await
}

pub(crate) async fn unlink(
    client: &mut Client,
    user_id: &str,
    source_ip: &str,
    now: u64,
) -> Result<FencedWriteOutcome, StoreError> {
    mutate(client, user_id, None, source_ip, now).await
}

async fn mutate(
    client: &mut Client,
    user_id: &str,
    replacement: Option<&StoredLink>,
    source_ip: &str,
    now: u64,
) -> Result<FencedWriteOutcome, StoreError> {
    let now = i64::try_from(now).map_err(|_| StoreError)?;
    let transaction = client.transaction().await.map_err(|_| StoreError)?;
    // FOR UPDATE serializes absent-row inserts, relinks and unlinks. The
    // same lock conflicts with scheduling deletion and with user erasure.
    let owners = transaction
        .query_typed(
            "SELECT deleting_at IS NOT NULL AS deleting FROM users WHERE id = $1 FOR UPDATE",
            &[(&user_id, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    if owners.len() > 1 {
        return Err(StoreError);
    }
    let Some(owner) = owners.first() else {
        return Ok(FencedWriteOutcome::NoRow);
    };
    if owner
        .try_get::<_, bool>("deleting")
        .map_err(|_| StoreError)?
    {
        return Ok(FencedWriteOutcome::AccountDeleting);
    }
    let removed = transaction
        .query_typed(
            "DELETE FROM listenbrainz_links WHERE user_id = $1 RETURNING user_id",
            &[(&user_id, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    if removed.len() > 1 {
        return Err(StoreError);
    }
    let action = if let Some(link) = replacement {
        let inserted = transaction
            .query_typed(
                "INSERT INTO listenbrainz_links \
                 (user_id, username, kek_version, dek_wrapped_nonce, dek_wrapped, \
                  payload_nonce, ciphertext, created_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING user_id",
                &[
                    (&user_id, Type::TEXT),
                    (&link.username.as_str(), Type::TEXT),
                    (
                        &i32::try_from(link.kek_version).map_err(|_| StoreError)?,
                        Type::INT4,
                    ),
                    (&link.envelope.dek_wrapped_nonce.as_slice(), Type::BYTEA),
                    (&link.envelope.dek_wrapped, Type::BYTEA),
                    (&link.envelope.payload_nonce.as_slice(), Type::BYTEA),
                    (&link.envelope.ciphertext, Type::BYTEA),
                    (&now, Type::INT8),
                ],
            )
            .await
            .map_err(|_| StoreError)?;
        if inserted.len() != 1 {
            return Err(StoreError);
        }
        if removed.is_empty() {
            "listenbrainz.link"
        } else {
            "listenbrainz.relink"
        }
    } else {
        "listenbrainz.unlink"
    };
    // No username, token, envelope, key version or provider response in audit.
    transaction
        .query_typed(
            "INSERT INTO audit_log (user_id, action, ok, source_ip, at, subject_user_id, details) \
             VALUES ($1, $2, TRUE, $3, $4, NULL, '{}'::jsonb)",
            &[
                (&user_id, Type::TEXT),
                (&action, Type::TEXT),
                (
                    &crate::vault::sanitize_source_ip(Some(source_ip)),
                    Type::TEXT,
                ),
                (&now, Type::INT8),
            ],
        )
        .await
        .map_err(|_| StoreError)?;
    transaction.commit().await.map_err(|_| StoreError)?;
    // Unlinking an already-unlinked account is idempotent success.
    Ok(FencedWriteOutcome::Written)
}

/// Wrap-only compare-and-set. Moves only the DEK wrap and KEK version from
/// `captured` to `replacement`; the payload is never written. The owner row is
/// locked first (as in replace/unlink), so a concurrent relink, unlink or
/// erasure either finishes before this reads the link or waits behind it. The
/// UPDATE matches the whole captured envelope and username, so a same-version
/// relink or any other change loses (`ClaimLost`) instead of being overwritten.
pub(crate) async fn cas_rewrap(
    client: &mut Client,
    user_id: &str,
    captured: &StoredLink,
    replacement: &StoredLink,
    now: u64,
) -> Result<RewrapOutcome, StoreError> {
    // Validate before opening a transaction.
    check_rewrap_args(user_id, captured, replacement)?;
    let transaction = client.transaction().await.map_err(|_| StoreError)?;
    let outcome = cas_rewrap_in(&transaction, user_id, captured, replacement, now).await?;
    transaction.commit().await.map_err(|_| StoreError)?;
    Ok(outcome)
}

fn check_rewrap_args(
    user_id: &str,
    captured: &StoredLink,
    replacement: &StoredLink,
) -> Result<(), StoreError> {
    check_shape(user_id, captured)?;
    check_shape(user_id, replacement)?;
    if captured.username != replacement.username
        || captured.envelope.payload_nonce != replacement.envelope.payload_nonce
        || captured.envelope.ciphertext != replacement.envelope.ciphertext
        || replacement.kek_version <= captured.kek_version
    {
        return Err(StoreError);
    }
    Ok(())
}

/// The CAS body on an already-open transaction. It never begins, commits or
/// rolls back: the caller owns the transaction. A non-`Rewrapped` outcome
/// wrote nothing. On `Err(StepError::Sql)` the transaction is aborted.
pub(crate) async fn cas_rewrap_in(
    transaction: &impl GenericClient,
    user_id: &str,
    captured: &StoredLink,
    replacement: &StoredLink,
    now: u64,
) -> Result<RewrapOutcome, StepError> {
    check_rewrap_args(user_id, captured, replacement).map_err(|_| StepError::Data)?;
    let now = i64::try_from(now).map_err(|_| StepError::Data)?;
    let old_version = i32::try_from(captured.kek_version).map_err(|_| StepError::Data)?;
    let new_version = i32::try_from(replacement.kek_version).map_err(|_| StepError::Data)?;
    let owners = transaction
        .query_typed(
            "SELECT deleting_at IS NOT NULL AS deleting FROM users WHERE id = $1 FOR UPDATE",
            &[(&user_id, Type::TEXT)],
        )
        .await
        .map_err(|_| StepError::Sql)?;
    if owners.len() > 1 {
        return Err(StepError::Data);
    }
    let Some(owner) = owners.first() else {
        return Ok(RewrapOutcome::NoRow);
    };
    if owner
        .try_get::<_, bool>("deleting")
        .map_err(|_| StepError::Data)?
    {
        return Ok(RewrapOutcome::AccountDeleting);
    }
    let updated = transaction
        .query_typed(
            "UPDATE listenbrainz_links SET kek_version = $1, dek_wrapped_nonce = $2, \
             dek_wrapped = $3 WHERE user_id = $4 AND username = $5 AND kek_version = $6 \
             AND dek_wrapped_nonce = $7 AND dek_wrapped = $8 AND payload_nonce = $9 \
             AND ciphertext = $10 RETURNING user_id",
            &[
                (&new_version, Type::INT4),
                (
                    &replacement.envelope.dek_wrapped_nonce.as_slice(),
                    Type::BYTEA,
                ),
                (&replacement.envelope.dek_wrapped, Type::BYTEA),
                (&user_id, Type::TEXT),
                (&captured.username.as_str(), Type::TEXT),
                (&old_version, Type::INT4),
                (&captured.envelope.dek_wrapped_nonce.as_slice(), Type::BYTEA),
                (&captured.envelope.dek_wrapped, Type::BYTEA),
                (&captured.envelope.payload_nonce.as_slice(), Type::BYTEA),
                (&captured.envelope.ciphertext, Type::BYTEA),
            ],
        )
        .await
        .map_err(|_| StepError::Sql)?;
    match updated.len() {
        0 => return Ok(RewrapOutcome::ClaimLost),
        1 => {}
        // Multi-row write is not a per-row data issue: abort the transaction.
        _ => return Err(StepError::Sql),
    }
    // Scalar-only audit: no username, envelope or key version.
    transaction
        .query_typed(
            "INSERT INTO audit_log (user_id, action, ok, source_ip, at, subject_user_id, details) \
             VALUES ($1, 'listenbrainz.rewrap', TRUE, $2, $3, NULL, '{}'::jsonb)",
            &[
                (&user_id, Type::TEXT),
                (&crate::vault::sanitize_source_ip(None), Type::TEXT),
                (&now, Type::INT8),
            ],
        )
        .await
        .map_err(|_| StepError::Sql)?;
    Ok(RewrapOutcome::Rewrapped)
}

#[cfg(all(test, feature = "postgres-tests", not(target_arch = "wasm32")))]
mod postgres_tests;
