use nntune_ingest::multipart::session_outcome::CompletionScope;
use tokio_postgres::{types::Type, Client};

use crate::storage_digest_read::uuid_text;

use super::TrialError::{self, Unconfirmed};

/// Resolve only on the upload writer after live preflight. A registered
/// incomplete attempt must be reconciled before any further provider writes.
/// Shared namespace eligibility is checked by the registrar and authority reader.
pub async fn registered_header(
    writer: &Client,
    scope: &CompletionScope<'_>,
    identities: [[u8; 16]; 5],
    map: [u8; 16],
    job: &str,
) -> Result<Option<[u8; 48]>, TrialError> {
    let [environment, library, object, generation, configuration] = identities.map(uuid_text);
    let row = writer.query_typed_opt(
        "SELECT i.header_tag
         FROM public.ingest_session_reservations s
         JOIN public.storage_library_namespaces l ON s.library_id = l.id
         LEFT JOIN public.storage_object_intents i
           ON i.object_id = s.multipart_object_id AND i.generation_id = s.checkpoint_generation
          AND i.object_id = $6::text::uuid AND i.generation_id = $7::text::uuid
          AND i.provider_configuration_version_id = $8::text::uuid AND s.completed_map_id = $9::text::uuid
          AND i.environment_id = l.environment_id AND i.library_id = l.id AND i.plaintext_bytes = $11
          AND s.terminal_reason = 'completed' AND s.checkpoint_revision = 1 AND s.checkpoint_offset = $11
          AND s.id = $1::text::uuid AND s.fingerprint = $3
         WHERE l.environment_id = $4::text::uuid AND l.id = $5::text::uuid
           AND s.principal_id = $2 AND s.idempotency_key = $10",
        &[(&scope.session, Type::TEXT), (&scope.principal, Type::TEXT), (&scope.fingerprint.as_slice(), Type::BYTEA),
          (&environment, Type::TEXT), (&library, Type::TEXT), (&object, Type::TEXT), (&generation, Type::TEXT),
          (&configuration, Type::TEXT), (&uuid_text(map), Type::TEXT), (&job, Type::TEXT),
          (&(super::FIXTURE_BYTES as i64), Type::INT8)],
    ).await.map_err(|_| Unconfirmed)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let header = row
        .try_get::<_, Option<&[u8]>>("header_tag")
        .map_err(|_| Unconfirmed)?
        .ok_or(Unconfirmed)?;
    Ok(Some(header.try_into().map_err(|_| Unconfirmed)?))
}
