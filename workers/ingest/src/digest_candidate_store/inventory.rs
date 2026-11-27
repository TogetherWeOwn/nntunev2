use tokio_postgres::{
    error::SqlState,
    types::{ToSql, Type},
    Error, Transaction,
};

use super::{
    candidate_geometry, candidate_hashes, uuid, valid_inventory_versions, CandidateIntent, LOCK_SQL,
};
use crate::multipart::readback_inventory::{ReadbackScope, VerifiedCompletedReadback};

// The claim FK binds the digest, and ordered hashes bind the constrained map pages.
const MATCH_SQL: &str = "WITH matching_candidate AS MATERIALIZED (
    SELECT m.map_id, r.object_id, r.environment_id, r.library_id, r.plaintext_bytes,
           r.plaintext_sha256, r.expires_at
    FROM public.storage_digest_reservations r
    JOIN public.storage_object_intents i
      USING (object_id, environment_id, library_id, plaintext_bytes)
    JOIN public.storage_digest_map_intents m
      USING (object_id, environment_id, library_id, plaintext_bytes)
    WHERE current_setting('transaction_isolation') = 'read committed'
      AND r.environment_id = $1::text::uuid AND r.library_id = $2::text::uuid
      AND r.plaintext_sha256 = $3 AND r.plaintext_bytes = $4
      AND r.object_id = $5::text::uuid AND r.generation = $6
      AND i.generation_id = $7::text::uuid AND i.provider_configuration_version_id = $8::text::uuid
      AND i.header_tag = $9 AND m.map_id = $10::text::uuid
      AND m.page_count = cardinality($11::bytea[])
      AND ARRAY(SELECT p.expected_hash FROM public.storage_digest_map_page_intents p
                WHERE p.map_id = m.map_id ORDER BY p.page_index) = $11
) ";

const APPEND_SQL: &str =
    "INSERT INTO public.storage_digest_inventory_intents (map_id, media_version, page_versions)
    SELECT map_id, $12::text, $13::text[] FROM matching_candidate c
    WHERE expires_at > pg_catalog.clock_timestamp()
      AND NOT EXISTS (SELECT FROM public.storage_validated_digest_winners w
                      WHERE w.object_id = c.object_id)
    RETURNING 1";

// A conflicting object or active scoped winner violates the existing unique keys.
const STAGE_WINNER_SQL: &str = "INSERT INTO public.storage_validated_digest_winners
        (object_id, environment_id, library_id, plaintext_bytes, plaintext_sha256,
         root_version_id, retain_until)
    SELECT c.object_id, c.environment_id, c.library_id, c.plaintext_bytes, c.plaintext_sha256,
           e.root_version_id, pg_catalog.to_timestamp($16::bigint::double precision)
    FROM matching_candidate c
    JOIN public.storage_digest_inventory_intents p USING (map_id)
    JOIN public.storage_object_envelope_intents e USING (object_id)
    WHERE p.media_version = $12::text AND p.page_versions = $13::text[]
      AND e.root_version_id = $14::text::uuid AND e.wrapped_record = $15::bytea
      AND c.expires_at > pg_catalog.clock_timestamp()
      AND pg_catalog.to_timestamp($16::bigint::double precision) > pg_catalog.clock_timestamp()
    RETURNING 1";

// This separate statement runs after all insert triggers and foreign-key checks.
const STILL_LIVE_SQL: &str = "SELECT 1 FROM public.storage_digest_reservations
    WHERE environment_id = $1::text::uuid AND library_id = $2::text::uuid
      AND plaintext_sha256 = $3 AND plaintext_bytes = $4
      AND object_id = $5::text::uuid AND generation = $6
      AND expires_at > pg_catalog.clock_timestamp()";

#[derive(Debug, PartialEq, Eq)]
pub enum InventoryAppendError {
    Identity,
    MissingOrFenced,
    Conflict,
    Indeterminate,
}

impl InventoryAppendError {
    fn from_db(error: Error) -> Self {
        if error.code() == Some(&SqlState::UNIQUE_VIOLATION) {
            Self::Conflict
        } else {
            Self::Indeterminate
        }
    }
}

enum Write {
    AppendInventory,
    StageWinner(i64),
}

/// Stage an inactive inventory row in the caller's READ COMMITTED write transaction.
/// The caller must already authorize the exact candidate and supply independently
/// established bucket/configuration custody. This helper takes or retains the
/// digest reservation lock and checks the live holder before appending.
/// The caller owns deadlines and commit. Roll back on cancellation or an
/// indeterminate result. Success only stages a row. Failures grant no retry,
/// reuse or publication authority.
pub async fn append_inventory(
    tx: &mut Transaction<'_>,
    intent: &CandidateIntent,
    scope: ReadbackScope<'_>,
    verified: &VerifiedCompletedReadback<'_, '_>,
) -> Result<(), InventoryAppendError> {
    write_verified(tx, intent, scope, verified, Write::AppendInventory).await
}

/// Stage a validated winner in the caller's READ COMMITTED write transaction.
/// The caller must authorize the exact intent, bucket/configuration and root
/// custody, establish the separate session fence, and supply the authorized
/// retention deadline. The inventory may already be staged in this transaction.
/// The caller owns prior effects, deadlines and the transaction outcome.
/// Roll back the outer transaction on cancellation or an indeterminate result.
pub async fn stage_validated_winner(
    tx: &mut Transaction<'_>,
    intent: &CandidateIntent,
    scope: ReadbackScope<'_>,
    verified: &VerifiedCompletedReadback<'_, '_>,
    retain_until_unix_seconds: i64,
) -> Result<(), InventoryAppendError> {
    write_verified(
        tx,
        intent,
        scope,
        verified,
        Write::StageWinner(retain_until_unix_seconds),
    )
    .await
}

/// Stage the exact receipt pins and their winner as one unit. The caller must
/// already hold the authorized upload session fence, acquired before the
/// digest reservation, and supply trusted custody and retention. Use a bounded
/// READ COMMITTED transaction and roll it back on cancellation or uncertainty.
/// Success is staged only. The caller must record the scoped session outcome
/// and recheck its fences before committing the outer transaction.
pub async fn stage_completed_candidate(
    tx: &mut Transaction<'_>,
    intent: &CandidateIntent,
    scope: ReadbackScope<'_>,
    verified: &VerifiedCompletedReadback<'_, '_>,
    retain_until_unix_seconds: i64,
) -> Result<(), InventoryAppendError> {
    let mut staged = tx
        .transaction()
        .await
        .map_err(|_| InventoryAppendError::Indeterminate)?;
    let result = async {
        check_retention(&staged, retain_until_unix_seconds).await?;
        match append_inventory(&mut staged, intent, scope, verified).await {
            // The winner guard below compares every pin, including when the
            // append reports a pre-existing inventory row.
            Ok(()) | Err(InventoryAppendError::Conflict) => {}
            Err(error) => return Err(error),
        }
        stage_validated_winner(
            &mut staged,
            intent,
            scope,
            verified,
            retain_until_unix_seconds,
        )
        .await?;
        check_retention(&staged, retain_until_unix_seconds).await
    }
    .await;
    match result {
        Ok(()) => staged
            .commit()
            .await
            .map_err(|_| InventoryAppendError::Indeterminate),
        Err(error) => {
            staged
                .rollback()
                .await
                .map_err(|_| InventoryAppendError::Indeterminate)?;
            Err(error)
        }
    }
}

async fn check_retention(
    tx: &Transaction<'_>,
    retain_until_unix_seconds: i64,
) -> Result<(), InventoryAppendError> {
    tx.query_typed_opt(
        "SELECT 1 WHERE pg_catalog.to_timestamp($1::bigint::double precision)
            > pg_catalog.clock_timestamp()",
        &[(&retain_until_unix_seconds, Type::INT8)],
    )
    .await
    .map_err(|_| InventoryAppendError::Indeterminate)?
    .ok_or(InventoryAppendError::MissingOrFenced)?;
    Ok(())
}

async fn write_verified(
    tx: &mut Transaction<'_>,
    intent: &CandidateIntent,
    scope: ReadbackScope<'_>,
    verified: &VerifiedCompletedReadback<'_, '_>,
    write: Write,
) -> Result<(), InventoryAppendError> {
    use InventoryAppendError::{Identity, MissingOrFenced};

    let inventory = verified.inventory();
    if scope.bucket != inventory.bucket() || scope.configuration != intent.identities[4] {
        return Err(Identity);
    }
    let origin = verified.origin();
    let geometry = candidate_geometry(intent.plaintext_len).ok_or(Identity)?;
    let hashes: Vec<&[u8]> = candidate_hashes(intent, origin)
        .map_err(|_| Identity)?
        .iter()
        .map(|hash| hash.as_slice())
        .collect();
    let media_version = inventory.media().version.ok_or(Identity)?;
    let page_versions = inventory
        .pages()
        .iter()
        .map(|(_, pin)| pin.version)
        .collect::<Option<Vec<_>>>()
        .ok_or(Identity)?;
    if !valid_inventory_versions(geometry, media_version, &page_versions) {
        return Err(Identity);
    }
    let [environment, library, object, sealing_generation, provider] =
        intent.identities.map(|id| uuid(&id));
    let digest = origin.plaintext_sha256().as_slice();
    let length = geometry.plaintext_len() as i64;

    let map = uuid(&intent.map_id);
    let header = intent.header.as_slice();
    let mut params: Vec<(&(dyn ToSql + Sync), Type)> = vec![
        (&environment, Type::TEXT),
        (&library, Type::TEXT),
        (&digest, Type::BYTEA),
        (&length, Type::INT8),
        (&object, Type::TEXT),
        (&intent.reservation_generation, Type::INT8),
        (&sealing_generation, Type::TEXT),
        (&provider, Type::TEXT),
        (&header, Type::BYTEA),
        (&map, Type::TEXT),
        (&hashes, Type::BYTEA_ARRAY),
        (&media_version, Type::TEXT),
        (&page_versions, Type::TEXT_ARRAY),
    ];
    let (root_version, record) = verified.authenticated_envelope();
    let root_version = uuid(root_version);
    let wrapped_record = record.as_slice();
    let action = match &write {
        Write::AppendInventory => APPEND_SQL,
        Write::StageWinner(retain_until_unix_seconds) => {
            params.push((&root_version, Type::TEXT));
            params.push((&wrapped_record, Type::BYTEA));
            params.push((retain_until_unix_seconds, Type::INT8));
            STAGE_WINNER_SQL
        }
    };

    let savepoint = tx
        .transaction()
        .await
        .map_err(InventoryAppendError::from_db)?;
    let result = async {
        let held = savepoint.query_typed_opt(LOCK_SQL, &params[..4]).await?;
        // READ COMMITTED refreshes the snapshot after the holder releases the lock.
        if held.is_none()
            || savepoint
                .query_typed_opt(&format!("{MATCH_SQL}{action}"), &params)
                .await?
                .is_none()
        {
            return Ok(false);
        }
        Ok(savepoint
            .query_typed_opt(STILL_LIVE_SQL, &params[..6])
            .await?
            .is_some())
    }
    .await;
    let result = match (result, write) {
        (Ok(true), Write::StageWinner(until)) => check_retention(&savepoint, until).await,
        (Ok(true), Write::AppendInventory) => Ok(()),
        (Ok(false), _) => Err(MissingOrFenced),
        (Err(error), _) => Err(InventoryAppendError::from_db(error)),
    };
    if result.is_ok() {
        return savepoint
            .commit()
            .await
            .map_err(InventoryAppendError::from_db);
    }
    savepoint
        .rollback()
        .await
        .map_err(InventoryAppendError::from_db)?;
    result
}
