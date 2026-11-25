use nntune_ingest::multipart::session_outcome::CompletionScope;
use tokio_postgres::{types::Type, Client};

use crate::storage_digest_read::uuid_text;

use super::TrialError::{self, Unconfirmed};

/// Resolve only on the upload writer after live preflight. A registered
/// incomplete attempt must be reconciled before any further provider writes.
pub async fn registered_header(
    writer: &Client,
    scope: &CompletionScope<'_>,
    identities: [[u8; 16]; 5],
    map: [u8; 16],
    job: &str,
) -> Result<Option<[u8; 48]>, TrialError> {
    let [environment, library, object, generation, configuration] = identities.map(uuid_text);
    let row = writer.query_typed_opt(
        "SELECT i.header_tag, s.terminal_reason IS DISTINCT FROM 'completed' AS incomplete
         FROM public.ingest_session_reservations s
         JOIN public.storage_library_namespaces l ON s.library_id = l.id
         LEFT JOIN public.storage_object_intents i
           ON i.object_id = s.multipart_object_id AND i.generation_id = s.checkpoint_generation
          AND i.object_id = $6::text::uuid AND i.generation_id = $7::text::uuid
          AND i.provider_configuration_version_id = $8::text::uuid AND s.completed_map_id = $9::text::uuid
         WHERE s.id = $1::text::uuid AND s.principal_id = $2 AND s.fingerprint = $3
           AND l.environment_id = $4::text::uuid AND l.id = $5::text::uuid AND s.idempotency_key = $10",
        &[(&scope.session, Type::TEXT), (&scope.principal, Type::TEXT), (&scope.fingerprint.as_slice(), Type::BYTEA),
          (&environment, Type::TEXT), (&library, Type::TEXT), (&object, Type::TEXT), (&generation, Type::TEXT),
          (&configuration, Type::TEXT), (&uuid_text(map), Type::TEXT), (&job, Type::TEXT)],
    ).await.map_err(|_| Unconfirmed)?;
    let Some(row) = row else {
        return Ok(None);
    };
    if row
        .try_get::<_, bool>("incomplete")
        .map_err(|_| Unconfirmed)?
    {
        return Err(Unconfirmed);
    }
    let header: &[u8] = row.try_get("header_tag").map_err(|_| Unconfirmed)?;
    Ok(Some(header.try_into().map_err(|_| Unconfirmed)?))
}
