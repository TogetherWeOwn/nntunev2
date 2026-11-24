//! Reconcile the result of an already committed private trial attempt.

use tokio_postgres::{types::Type, Client};

use crate::{
    handlers::StoreError,
    ingest_session_caller::{
        self, Authorization, CurrentAuthorization, PreparedUpload, UploadError,
    },
    storage_digest_read::uuid_text,
};
use nntune_ingest::multipart::{session_outcome::CompletionScope, session_upload::begin};

use super::{
    CommittedPublication, Permit,
    TrialError::{self, Refused, Unconfirmed},
};

pub async fn read_registered_header(
    reader: &Client,
    scope: &CompletionScope<'_>,
    identities: [[u8; 16]; 5],
    map: [u8; 16],
) -> Result<Option<[u8; 48]>, TrialError> {
    let [environment, library, object, generation, configuration] = identities.map(uuid_text);
    let row = reader
        .query_typed_opt(
            "SELECT i.header_tag FROM public.ingest_session_reservations s
         JOIN public.storage_object_intents i
           ON i.object_id = s.multipart_object_id AND i.generation_id = s.checkpoint_generation
         JOIN public.storage_library_namespaces l ON s.library_id = l.id AND l.owner_id IS NULL
         WHERE s.id = $1::text::uuid AND s.principal_id = $2 AND s.fingerprint = $3
           AND l.environment_id = $4::text::uuid AND l.id = $5::text::uuid
           AND i.object_id = $6::text::uuid AND i.generation_id = $7::text::uuid
           AND i.provider_configuration_version_id = $8::text::uuid
           AND s.completed_map_id = $9::text::uuid AND s.terminal_reason = 'completed'",
            &[
                (&scope.session, Type::TEXT),
                (&scope.principal, Type::TEXT),
                (&scope.fingerprint.as_slice(), Type::BYTEA),
                (&environment, Type::TEXT),
                (&library, Type::TEXT),
                (&object, Type::TEXT),
                (&generation, Type::TEXT),
                (&configuration, Type::TEXT),
                (&uuid_text(map), Type::TEXT),
            ],
        )
        .await
        .map_err(|_| Unconfirmed)?;
    row.map(|row| {
        row.try_get::<_, &[u8]>(0)
            .map_err(|_| Unconfirmed)?
            .try_into()
            .map_err(|_| Unconfirmed)
    })
    .transpose()
}

impl Permit<'_> {
    pub async fn replay(
        self,
        publisher: &mut Client,
        writer: &mut Client,
        bucket: &str,
        auth: Authorization<'_>,
        prepared: PreparedUpload<'_>,
        authority: impl AsyncFnOnce(
            &str,
            &CompletionScope<'_>,
        ) -> Result<Option<CurrentAuthorization>, StoreError>,
    ) -> Result<CommittedPublication, TrialError> {
        if prepared.scope.principal != self.actor.user_id
            || prepared.request.content_sha256 != self.digest
            || self.policy.library_scope.as_ref().is_none_or(|scope| {
                !scope.matches(prepared.scope.environment, prepared.scope.library)
            })
        {
            return Err(Refused);
        }
        let tx = begin(publisher).await.map_err(|_| Unconfirmed)?;
        let recipient = self.recipient(&tx).await?;
        let committed = ingest_session_caller::read(writer, auth, prepared, authority)
            .await
            .map_err(|error| match error {
                UploadError::Refused => Refused,
                UploadError::Unconfirmed => Unconfirmed,
            })?
            .ok_or(Unconfirmed)?;
        if committed.root_version.replace('-', "") != uuid_text(prepared.context.identities[0])
            || committed.map.replace('-', "") != uuid_text(prepared.intent.map_id)
            || committed.object.replace('-', "") != uuid_text(prepared.intent.identities[2])
            || committed.generation.replace('-', "") != uuid_text(prepared.intent.identities[3])
            || committed.configuration.replace('-', "") != uuid_text(prepared.intent.identities[4])
        {
            return Err(Unconfirmed);
        }
        let row = tx
            .query_typed_opt(
                include_str!("replay.sql"),
                &[
                    (&self.target.invite_id, Type::INT8),
                    (&recipient, Type::TEXT),
                    (&self.target.job_id, Type::TEXT),
                    (&self.actor.user_id, Type::TEXT),
                    (&uuid_text(prepared.scope.environment), Type::TEXT),
                    (&uuid_text(prepared.scope.library), Type::TEXT),
                    (&committed.object, Type::TEXT),
                    (&committed.generation, Type::TEXT),
                    (&committed.map, Type::TEXT),
                    (&committed.root_version, Type::TEXT),
                    (&committed.configuration, Type::TEXT),
                    (&committed.media_version, Type::TEXT),
                    (&committed.page_versions, Type::TEXT_ARRAY),
                    (&bucket, Type::TEXT),
                    (&self.digest.as_slice(), Type::BYTEA),
                ],
            )
            .await
            .map_err(|_| Unconfirmed)?
            .ok_or(Unconfirmed)?;
        let track_id = row.try_get::<_, String>(0).map_err(|_| Unconfirmed)?;
        self.recipient(&tx).await?;
        tx.commit().await.map_err(|_| Unconfirmed)?;
        Ok(CommittedPublication {
            track_id,
            digest: self.digest,
        })
    }
}
