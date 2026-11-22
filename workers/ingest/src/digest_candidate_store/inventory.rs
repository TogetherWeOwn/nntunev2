use tokio_postgres::{error::SqlState, types::ToSql, Error, Transaction};

use super::{candidate_hashes, uuid, CandidateIntent, LOCK_SQL};
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
), ";

const APPEND_SQL: &str = "appended AS (
    INSERT INTO public.storage_digest_inventory_intents (map_id, media_version, page_versions)
    SELECT map_id, $12::text, $13::text[] FROM matching_candidate c
    WHERE expires_at > clock_timestamp()
      AND NOT EXISTS (SELECT FROM public.storage_validated_digest_winners w
                      WHERE w.object_id = c.object_id)
    RETURNING map_id
)
SELECT expires_at > clock_timestamp() FROM appended JOIN matching_candidate USING (map_id)";

// A conflicting object or active scoped winner violates the existing unique keys.
// Recheck liveness after the insert, which can itself wait on database locks.
const STAGE_WINNER_SQL: &str = "staged AS (
    INSERT INTO public.storage_validated_digest_winners
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
    RETURNING object_id
)
SELECT expires_at > pg_catalog.clock_timestamp() FROM staged JOIN matching_candidate USING (object_id)";

#[derive(Debug, PartialEq, Eq)]
pub enum VerifiedWriteError {
    Identity,
    MissingOrFenced,
    Conflict,
    Indeterminate,
}

impl VerifiedWriteError {
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
    StageWinner { retain_until_unix_seconds: i64 },
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
) -> Result<(), VerifiedWriteError> {
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
) -> Result<(), VerifiedWriteError> {
    write_verified(
        tx,
        intent,
        scope,
        verified,
        Write::StageWinner {
            retain_until_unix_seconds,
        },
    )
    .await
}

async fn write_verified(
    tx: &mut Transaction<'_>,
    intent: &CandidateIntent,
    scope: ReadbackScope<'_>,
    verified: &VerifiedCompletedReadback<'_, '_>,
    write: Write,
) -> Result<(), VerifiedWriteError> {
    use VerifiedWriteError::{Identity, MissingOrFenced};

    let inventory = verified.inventory();
    if scope.bucket != inventory.bucket() || scope.configuration != intent.identities[4] {
        return Err(Identity);
    }
    let origin = verified.origin();
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
    let [environment, library, object, sealing_generation, provider] =
        intent.identities.map(|id| uuid(&id));
    let digest = origin.plaintext_sha256().as_slice();
    let length = i64::try_from(intent.plaintext_len).map_err(|_| Identity)?;

    let map = uuid(&intent.map_id);
    let header = intent.header.as_slice();
    let mut params: Vec<&(dyn ToSql + Sync)> = vec![
        &environment,
        &library,
        &digest,
        &length,
        &object,
        &intent.reservation_generation,
        &sealing_generation,
        &provider,
        &header,
        &map,
        &hashes,
        &media_version,
        &page_versions,
    ];
    let (root_version, record) = verified.authenticated_envelope();
    let root_version = uuid(root_version);
    let wrapped_record = record.as_slice();
    let action = match &write {
        Write::AppendInventory => APPEND_SQL,
        Write::StageWinner {
            retain_until_unix_seconds,
        } => {
            params.extend([
                &root_version as &(dyn ToSql + Sync),
                &wrapped_record,
                retain_until_unix_seconds,
            ]);
            STAGE_WINNER_SQL
        }
    };

    let savepoint = tx
        .transaction()
        .await
        .map_err(VerifiedWriteError::from_db)?;
    let held = savepoint
        .query_opt(LOCK_SQL, &[&environment, &library, &digest, &length])
        .await;
    // READ COMMITTED sees the prior holder's changes after the lock wait.
    let result = match held {
        Ok(Some(_)) => savepoint
            .query_opt(&format!("{MATCH_SQL}{action}"), &params)
            .await
            .and_then(|row| row.map(|row| row.try_get::<_, bool>(0)).transpose()),
        Ok(None) => Ok(None),
        Err(error) => Err(error),
    };
    if matches!(result, Ok(Some(true))) {
        return savepoint
            .commit()
            .await
            .map_err(VerifiedWriteError::from_db);
    }
    savepoint
        .rollback()
        .await
        .map_err(VerifiedWriteError::from_db)?;
    Err(result
        .err()
        .map_or(MissingOrFenced, VerifiedWriteError::from_db))
}
