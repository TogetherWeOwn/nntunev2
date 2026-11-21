use tokio_postgres::{error::SqlState, Error, Transaction};

use super::{candidate_hashes, uuid, CandidateIntent, LOCK_SQL};
use crate::multipart::readback_inventory::{ReadbackScope, VerifiedCompletedReadback};

// The reservation FK binds the claim digest. Map/page constraints bind geometry
// and distinct page indices, so a complete ordered hash match binds every page.
const APPEND_SQL: &str = "WITH matching_candidate AS MATERIALIZED (
    SELECT m.map_id, r.expires_at
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
      AND NOT EXISTS (SELECT FROM public.storage_validated_digest_winners w
                      WHERE w.object_id = r.object_id)
), appended AS (
    INSERT INTO public.storage_digest_inventory_intents (map_id, media_version, page_versions)
    SELECT map_id, $12::text, $13::text[] FROM matching_candidate WHERE expires_at > clock_timestamp()
    RETURNING map_id
)
SELECT expires_at > clock_timestamp() FROM appended JOIN matching_candidate USING (map_id)";

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

/// Stage an inactive inventory row in the caller's READ COMMITTED write transaction.
/// The caller must already authorize the exact candidate and supply independently
/// established bucket/configuration custody. This helper takes or retains the
/// digest reservation lock and checks the live holder before appending.
///
/// The caller sets transaction deadlines, rolls back on error or cancellation,
/// and owns commit and its outcome. Success only describes a staged append.
/// Conflict and indeterminate results grant no retry, reuse or publication authority.
pub async fn append_inventory(
    tx: &mut Transaction<'_>,
    intent: &CandidateIntent,
    scope: ReadbackScope<'_>,
    verified: &VerifiedCompletedReadback<'_, '_>,
) -> Result<(), InventoryAppendError> {
    use InventoryAppendError::{Identity, MissingOrFenced};

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

    tx.query_opt(LOCK_SQL, &[&environment, &library, &digest, &length])
        .await
        .map_err(InventoryAppendError::from_db)?
        .ok_or(MissingOrFenced)?;

    // READ COMMITTED sees the prior lock holder's changes after a wait.
    let append = tx
        .transaction()
        .await
        .map_err(InventoryAppendError::from_db)?;
    let result = append
        .query_opt(
            APPEND_SQL,
            &[
                &environment,
                &library,
                &digest,
                &length,
                &object,
                &intent.reservation_generation,
                &sealing_generation,
                &provider,
                &intent.header.as_slice(),
                &uuid(&intent.map_id),
                &hashes,
                &media_version,
                &page_versions,
            ],
        )
        .await
        .and_then(|row| row.map(|row| row.try_get::<_, bool>(0)).transpose());
    if matches!(result, Ok(Some(true))) {
        return append.commit().await.map_err(InventoryAppendError::from_db);
    }
    append
        .rollback()
        .await
        .map_err(InventoryAppendError::from_db)?;
    Err(result
        .err()
        .map_or(MissingOrFenced, InventoryAppendError::from_db))
}
