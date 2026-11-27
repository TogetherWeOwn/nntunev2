//! Database evidence only; there is deliberately no runtime caller.

use std::future::Future;

use tokio_postgres::{Client, Error, IsolationLevel, Row};

use super::{
    candidate_geometry, candidate_hashes, uuid, CandidateIntent, OriginManifest, LOCK_SQL,
};
use crate::digest_candidate_identity::{candidate_pages_match, CandidatePage};

const OBSERVE_SQL: &str = "SELECT r.generation,
    r.object_id = i.object_id AND r.expires_at > clock_timestamp(),
    m.map_id IS NOT NULL,
    COALESCE((m.map_id, m.object_id, m.environment_id, m.library_id, m.plaintext_bytes,
              m.chunk_count, m.page_count) = ($9::text::uuid, i.object_id, i.environment_id,
              i.library_id, i.plaintext_bytes, $10, $11), FALSE),
    p.page_index, p.chunk_count, p.first_chunk, p.entry_count, p.expected_hash
 FROM public.storage_digest_reservations AS r
 JOIN public.storage_plaintext_digest_claim_intents AS c
   ON (c.environment_id, c.library_id, c.plaintext_sha256, c.plaintext_bytes)
    = (r.environment_id, r.library_id, r.plaintext_sha256, r.plaintext_bytes)
 JOIN public.storage_object_intents AS i
   ON (i.object_id, i.environment_id, i.library_id, i.plaintext_bytes)
    = (c.object_id, c.environment_id, c.library_id, c.plaintext_bytes)
 LEFT JOIN public.storage_digest_map_intents AS m
   ON m.object_id = i.object_id OR m.map_id = $9::text::uuid
 LEFT JOIN public.storage_digest_map_page_intents AS p ON p.map_id = m.map_id
 WHERE r.environment_id = $1::text::uuid AND r.library_id = $2::text::uuid
   AND r.plaintext_sha256 = $3 AND r.plaintext_bytes = $4
   AND i.object_id = $5::text::uuid AND i.generation_id = $6::text::uuid
   AND i.provider_configuration_version_id = $7::text::uuid AND i.header_tag = $8
   AND i.envelope_bytes = $12
 ORDER BY p.page_index LIMIT 513";

#[derive(Debug, PartialEq, Eq)]
pub(super) enum DatabaseOutcome {
    /// Exact inactive record under the live holder; not a receipt for a specific writer.
    MatchingCandidate,
    /// Neither identity has a candidate and a newer generation fences the old writer.
    FencedAbsent,
    Indeterminate,
}

/// Requires a fresh, dedicated autocommit connection to the trusted writer DB
/// with current append-only history, and the original sealer's manifest. These
/// observations cannot establish that trust or authenticated custody, and grant
/// no retry (including under a fresh generation), publication or reuse authority.
pub(super) async fn observe_with_deadline<D: Future<Output = ()>>(
    mut client: Client,
    intent: &CandidateIntent,
    manifest: &OriginManifest,
    deadline: D,
) -> DatabaseOutcome {
    let Ok(hashes) = candidate_hashes(intent, manifest) else {
        return DatabaseOutcome::Indeterminate;
    };
    let Some(geometry) = candidate_geometry(intent.plaintext_len) else {
        return DatabaseOutcome::Indeterminate;
    };
    let length = geometry.plaintext_len() as i64;
    let chunks = geometry.chunks() as i32;
    let pages = geometry.pages() as i32;
    let envelope_bytes = geometry.sealed_len() as i64;
    let [environment, library, object, sealing_generation, provider] =
        intent.identities.map(|id| uuid(&id));
    let digest = manifest.plaintext_sha256().as_slice();
    let header = intent.header.as_slice();
    let map = uuid(&intent.map_id);
    let operation = async move {
        let tx = client
            .build_transaction()
            .isolation_level(IsolationLevel::ReadCommitted)
            .read_only(false)
            .start()
            .await?;
        tx.batch_execute("SET LOCAL statement_timeout = '10s'; SET LOCAL idle_in_transaction_session_timeout = '10s'")
            .await?;
        let outcome = if tx
            .query_opt(LOCK_SQL, &[&environment, &library, &digest, &length])
            .await?
            .is_some()
        {
            // READ COMMITTED takes a fresh snapshot for this statement, after
            // the previous lock holder committed or rolled back. A CTE in the
            // lock statement would still read using its pre-wait snapshot.
            let rows = tx
                .query(
                    OBSERVE_SQL,
                    &[
                        &environment,
                        &library,
                        &digest,
                        &length,
                        &object,
                        &sealing_generation,
                        &provider,
                        &header,
                        &map,
                        &chunks,
                        &pages,
                        &envelope_bytes,
                    ],
                )
                .await?;
            classify(intent, hashes, &rows)?
        } else {
            DatabaseOutcome::Indeterminate
        };
        tx.rollback().await?;
        Ok::<_, Error>(outcome)
    };
    crate::library::with_db_deadline(async { operation.await.map_err(|_| ()) }, deadline, || ())
        .await
        .unwrap_or(DatabaseOutcome::Indeterminate)
}

fn classify(
    intent: &CandidateIntent,
    hashes: &[[u8; 32]],
    rows: &[Row],
) -> Result<DatabaseOutcome, Error> {
    use DatabaseOutcome::*;
    let Some(first) = rows.first() else {
        return Ok(Indeterminate);
    };
    let generation: i64 = first.try_get(0)?;
    if rows.len() == 1 && !first.try_get::<_, bool>(2)? {
        // Expiry alone is not monotonic: trusted lease writers may extend it
        // without changing generation. Only an observed successor proves a fence.
        return Ok(if generation > intent.reservation_generation {
            FencedAbsent
        } else {
            Indeterminate
        });
    }
    if generation != intent.reservation_generation || rows.len() != hashes.len() {
        return Ok(Indeterminate);
    }
    let mut pages = Vec::with_capacity(rows.len());
    for row in rows {
        if row.try_get::<_, i64>(0)? != generation
            || !row.try_get::<_, bool>(1)?
            || !row.try_get::<_, bool>(3)?
        {
            return Ok(Indeterminate);
        }
        pages.push(CandidatePage {
            index: row.try_get(4)?,
            chunk_count: row.try_get(5)?,
            first_chunk: row.try_get(6)?,
            entry_count: row.try_get(7)?,
            hash: row.try_get(8)?,
        });
    }
    Ok(
        if candidate_pages_match(intent.plaintext_len, hashes, &pages) {
            MatchingCandidate
        } else {
            Indeterminate
        },
    )
}
