//! Bounded PostgreSQL execution shared by native tests and Hyperdrive callers.
//! No connection, key lookup, route or source publication is installed here.

use tokio_postgres::{types::Type, Client, GenericClient};

pub mod owned;

/// Owning query adapter; use DrivenRepository to own and poll its driver too.
/// No connection is opened by construction and no query implementation is forked.
pub struct ClientRepository {
    client: Client,
}

impl ClientRepository {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

impl AsyncSourceRepository for ClientRepository {
    async fn metadata(
        &mut self,
        track_id: &str,
    ) -> Result<[Option<SourceMetadata>; 2], SourceServiceError> {
        PostgresSourceRepository::new(&self.client)
            .metadata(track_id)
            .await
    }

    async fn envelope(
        &mut self,
        selected: &SourceMetadata,
    ) -> Result<Option<FieldEnvelope>, SourceServiceError> {
        PostgresSourceRepository::new(&self.client)
            .envelope(selected)
            .await
    }
}

use super::{
    source_envelope_sql, source_metadata_sql, AsyncSourceRepository, FieldEnvelope, SourceMetadata,
    SourceServiceError,
};

/// Borrows the API's existing connection/transaction. Construction is inert;
/// the service authorizes before invoking either query. Unnamed typed queries
/// avoid prepared-statement names crossing Hyperdrive backend assignments.
pub struct PostgresSourceRepository<'a, C> {
    client: &'a C,
}

impl<'a, C: GenericClient> PostgresSourceRepository<'a, C> {
    pub fn new(client: &'a C) -> Self {
        Self { client }
    }
}

impl<C: GenericClient> AsyncSourceRepository for PostgresSourceRepository<'_, C> {
    async fn metadata(
        &mut self,
        track_id: &str,
    ) -> Result<[Option<SourceMetadata>; 2], SourceServiceError> {
        if !super::canonical_track_id(track_id) {
            return Err(SourceServiceError::Invalid);
        }
        let rows = self
            .client
            .query_typed(source_metadata_sql(), &[(&track_id, Type::TEXT)])
            .await
            .map_err(|_| SourceServiceError::Store)?;
        if rows.len() > 2 {
            return Err(SourceServiceError::Invalid);
        }
        let mut result = [None, None];
        for (slot, row) in result.iter_mut().zip(rows) {
            *slot = Some(SourceMetadata {
                track_id: row
                    .try_get("track_id")
                    .map_err(|_| SourceServiceError::Invalid)?,
                source_job_id: row
                    .try_get("source_job_id")
                    .map_err(|_| SourceServiceError::Invalid)?,
                envelope_version: row
                    .try_get("nzb_ref_envelope_version")
                    .map_err(|_| SourceServiceError::Invalid)?,
                payload_format: row
                    .try_get("nzb_ref_payload_format")
                    .map_err(|_| SourceServiceError::Invalid)?,
                kek_version: row
                    .try_get("nzb_ref_kek_version")
                    .map_err(|_| SourceServiceError::Invalid)?,
                ciphertext_len: row
                    .try_get("ciphertext_len")
                    .map_err(|_| SourceServiceError::Invalid)?,
                wrap_nonce_len: row
                    .try_get("wrap_nonce_len")
                    .map_err(|_| SourceServiceError::Invalid)?,
                wrapped_dek_len: row
                    .try_get("wrapped_dek_len")
                    .map_err(|_| SourceServiceError::Invalid)?,
                payload_nonce_len: row
                    .try_get("payload_nonce_len")
                    .map_err(|_| SourceServiceError::Invalid)?,
            });
        }
        Ok(result)
    }

    async fn envelope(
        &mut self,
        selected: &SourceMetadata,
    ) -> Result<Option<FieldEnvelope>, SourceServiceError> {
        // Defensive even for a caller outside read_original_source_async.
        selected.validate(&selected.track_id)?;
        let rows = self
            .client
            .query_typed(
                source_envelope_sql(),
                &[
                    (&selected.track_id, Type::TEXT),
                    (&selected.source_job_id, Type::TEXT),
                    (&selected.kek_version, Type::INT4),
                    (&selected.ciphertext_len, Type::INT4),
                ],
            )
            .await
            .map_err(|_| SourceServiceError::Store)?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let wrapped_nonce: Vec<u8> = row
            .try_get("nzb_ref_dek_wrapped_nonce")
            .map_err(|_| SourceServiceError::Invalid)?;
        let payload_nonce: Vec<u8> = row
            .try_get("nzb_ref_payload_nonce")
            .map_err(|_| SourceServiceError::Invalid)?;
        Ok(Some(FieldEnvelope {
            dek_wrapped_nonce: wrapped_nonce
                .try_into()
                .map_err(|_| SourceServiceError::Invalid)?,
            dek_wrapped: row
                .try_get("nzb_ref_dek_wrapped")
                .map_err(|_| SourceServiceError::Invalid)?,
            payload_nonce: payload_nonce
                .try_into()
                .map_err(|_| SourceServiceError::Invalid)?,
            ciphertext: row
                .try_get("nzb_ref_encrypted")
                .map_err(|_| SourceServiceError::Invalid)?,
        }))
    }
}
