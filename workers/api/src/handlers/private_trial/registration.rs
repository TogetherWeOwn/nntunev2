//! Durable enrollment of the one preselected staging upload.

use nntune_core::media::ciphertext_size;
use tokio_postgres::{types::Type, Client};

use crate::{
    handlers::StoreError,
    ingest_session_auth::{Action, Grant},
    ingest_session_caller::CurrentAuthorization,
    native_rest::{self, Pepper, Scope},
    storage_digest_read::uuid_text,
    worker_entry::native_credential_from_row,
    worker_entry::private_browser_worker::{hex, is_uuid_v4, uuid_v4},
};
use nntune_ingest::multipart::{session_outcome::CompletionScope, session_upload::begin};

use super::{sealed_fixture::SealedFixture, TrialError};

pub struct Enrollment<'a> {
    pub session: &'a str,
    pub job: &'a str,
    pub actor: &'a str,
    pub fingerprint: &'a [u8; 32],
}

impl Enrollment<'_> {
    /// Register the exact immutable candidate before claiming a provider upload.
    /// A previous registration must be reconciled or replayed by the caller.
    pub async fn register(
        &self,
        registrar: &mut Client,
        plaintext: &[u8],
        identities: [[u8; 16]; 5],
        map_id: [u8; 16],
        root_version: [u8; 16],
        root_key: &[u8; 32],
    ) -> Result<SealedFixture, TrialError> {
        use TrialError::{Refused, Unconfirmed};
        let session = uuid_v4(self.session).or_else(|| hex::<16>(self.session).filter(is_uuid_v4));
        if session.is_none()
            || !nntune_ingest::jobs::validate_job_id(self.job)
            || !(1..=128).contains(&self.actor.len())
            || self.actor.chars().any(char::is_control)
        {
            return Err(Refused);
        }
        let [environment, library, object, generation, configuration] = identities.map(uuid_text);
        let tx = begin(registrar).await.map_err(|_| Unconfirmed)?;
        let scope = tx
            .query_typed_opt(
                "SELECT j.accepted_seq
               FROM public.ingest_jobs j
               JOIN public.storage_library_namespaces n ON n.id = $3::text::uuid
              WHERE j.id = $1 AND j.user_id = $2 AND j.status = 'done'
                AND n.environment_id = $4::text::uuid AND n.owner_id IS NULL
              FOR SHARE OF j",
                &[
                    (&self.job, Type::TEXT),
                    (&self.actor, Type::TEXT),
                    (&library, Type::TEXT),
                    (&environment, Type::TEXT),
                ],
            )
            .await
            .map_err(|_| Unconfirmed)?;
        if scope.is_none() {
            return Err(Refused);
        }

        let inserted = tx.query_typed_opt(
            "INSERT INTO public.ingest_session_reservations
                (id, library_id, principal_id, idempotency_key, fingerprint, absolute_deadline, idle_deadline)
             VALUES ($1::text::uuid, $2::text::uuid, $3, $4, $5,
                     clock_timestamp() + interval '24 hours', clock_timestamp() + interval '30 minutes')
             ON CONFLICT DO NOTHING RETURNING 1",
            &[(&self.session, Type::TEXT), (&library, Type::TEXT), (&self.actor, Type::TEXT),
              (&self.job, Type::TEXT), (&self.fingerprint.as_slice(), Type::BYTEA)],
        ).await.map_err(|_| Unconfirmed)?;
        if inserted.is_none() {
            return Err(Refused);
        }

        // Only the reservation insert winner creates key material for this attempt.
        let mut fixture =
            SealedFixture::new(plaintext, identities, map_id, root_version, root_key)?;
        let intent = &fixture.intent;
        let map = uuid_text(intent.map_id);
        let root = uuid_text(root_version);
        let plain = i64::try_from(intent.plaintext_len).map_err(|_| Refused)?;
        let sealed = ciphertext_size(intent.plaintext_len)
            .and_then(|n| i64::try_from(n).ok())
            .ok_or(Refused)?;
        let hashes = fixture
            .origin()
            .candidate_page_hashes(
                &intent.identities,
                intent.plaintext_len,
                &intent.map_id,
                &intent.header,
            )
            .ok_or(Refused)?;
        let hashes = hashes
            .iter()
            .map(|hash| hash.as_slice())
            .collect::<Vec<_>>();

        tx.query_typed_one(
            "INSERT INTO public.ingest_session_cleanup_intents (session_id, kind, due_at)
             SELECT id, 'absolute_expiry', absolute_deadline
             FROM public.ingest_session_reservations WHERE id = $1::text::uuid RETURNING 1",
            &[(&self.session, Type::TEXT)],
        )
        .await
        .map_err(|_| Unconfirmed)?;
        tx.query_typed_one(
            "INSERT INTO public.storage_object_intents
               (object_id, generation_id, environment_id, library_id, provider_configuration_version_id,
                plaintext_bytes, envelope_bytes, header_tag)
             VALUES ($1::text::uuid, $2::text::uuid, $3::text::uuid, $4::text::uuid, $5::text::uuid,
                     $6, $7, $8) RETURNING 1",
            &[(&object, Type::TEXT), (&generation, Type::TEXT), (&environment, Type::TEXT),
              (&library, Type::TEXT), (&configuration, Type::TEXT), (&plain, Type::INT8),
              (&sealed, Type::INT8), (&intent.header.as_slice(), Type::BYTEA)],
        ).await.map_err(|_| Unconfirmed)?;
        tx.query_typed_one(
            "INSERT INTO public.storage_plaintext_digest_claim_intents
                (object_id, environment_id, library_id, plaintext_bytes, plaintext_sha256)
             VALUES ($1::text::uuid, $2::text::uuid, $3::text::uuid, $4, $5) RETURNING 1",
            &[
                (&object, Type::TEXT),
                (&environment, Type::TEXT),
                (&library, Type::TEXT),
                (&plain, Type::INT8),
                (&fixture.origin().plaintext_sha256().as_slice(), Type::BYTEA),
            ],
        )
        .await
        .map_err(|_| Unconfirmed)?;
        let reservation = tx
            .query_typed_one(
                "SELECT public.reserve_storage_digest($1::text::uuid, 3600)",
                &[(&object, Type::TEXT)],
            )
            .await
            .map_err(|_| Unconfirmed)?
            .try_get::<_, Option<i64>>(0)
            .map_err(|_| Unconfirmed)?
            .filter(|generation| *generation > 0)
            .ok_or(Refused)?;
        tx.query_typed_one(
            "SELECT public.store_storage_digest_candidate(
                 $1::text::uuid, $2::text::uuid, $3::text::uuid, $4::text::uuid, $5, $6)::text",
            &[
                (&environment, Type::TEXT),
                (&library, Type::TEXT),
                (&object, Type::TEXT),
                (&map, Type::TEXT),
                (&reservation, Type::INT8),
                (&hashes, Type::BYTEA_ARRAY),
            ],
        )
        .await
        .map_err(|_| Unconfirmed)?;
        tx.query_typed_one(
            "INSERT INTO public.storage_object_envelope_intents (object_id, root_version_id, wrapped_record)
             VALUES ($1::text::uuid, $2::text::uuid, $3) RETURNING 1",
            &[(&object, Type::TEXT), (&root, Type::TEXT), (&fixture.record.as_slice(), Type::BYTEA)],
        ).await.map_err(|_| Unconfirmed)?;
        tx.commit().await.map_err(|_| Unconfirmed)?;
        fixture.intent.reservation_generation = reservation;
        Ok(fixture)
    }
}

/// Use only the separate read binding. Call again under the scoped outcome
/// lock so a revoked credential or ended reservation cannot authorize replay.
pub async fn load_authority(
    reader: &Client,
    credential: &str,
    scope: &CompletionScope<'_>,
) -> Result<Option<CurrentAuthorization>, StoreError> {
    load(reader, credential, scope, true).await
}

/// Check the current credential before registering any fixture records.
pub async fn authorize_enrollment(
    reader: &Client,
    credential: &str,
    scope: &CompletionScope<'_>,
    bearer: &str,
    peppers: &[Pepper<'_>],
    now: u64,
) -> Result<(), TrialError> {
    let current = load(reader, credential, scope, false)
        .await
        .map_err(|_| TrialError::Unconfirmed)?
        .ok_or(TrialError::Refused)?;
    if now >= current.record.expires_at {
        return Err(TrialError::Refused);
    }
    native_rest::authorize(
        &current.record,
        bearer,
        Scope::IngestSubmit,
        now,
        scope.principal,
        peppers,
    )
    .map_err(|_| TrialError::Refused)
}

async fn load(
    reader: &Client,
    credential: &str,
    scope: &CompletionScope<'_>,
    require_reservation: bool,
) -> Result<Option<CurrentAuthorization>, StoreError> {
    let environment = uuid_text(scope.environment);
    let library = uuid_text(scope.library);
    let row = reader
        .query_typed_opt(
            "SELECT c.id, c.user_id, c.key_prefix, c.key_hmac, c.pepper_version, c.scopes,
                c.created_at, c.expires_at, c.revoked_at, NULL::bigint AS last_used_at,
                ''::text AS label, ''::text AS created_from_ip
           FROM public.native_rest_credentials c
          WHERE c.id = $1 AND c.user_id = $2 AND (NOT $7 OR EXISTS (
              SELECT FROM public.ingest_session_reservations s
              JOIN public.storage_library_namespaces n ON n.id = s.library_id
              WHERE s.principal_id = c.user_id AND s.id = $3::text::uuid
                AND n.environment_id = $4::text::uuid AND n.id = $5::text::uuid
                AND s.fingerprint = $6 AND n.owner_id IS NULL
                AND (s.terminal_reason IS NULL OR s.terminal_reason = 'completed')))",
            &[
                (&credential, Type::TEXT),
                (&scope.principal, Type::TEXT),
                (&scope.session, Type::TEXT),
                (&environment, Type::TEXT),
                (&library, Type::TEXT),
                (&scope.fingerprint.as_slice(), Type::BYTEA),
                (&require_reservation, Type::BOOL),
            ],
        )
        .await
        .map_err(|_| StoreError)?;
    row.map(|row| {
        let record = native_credential_from_row(&row)?.record;
        let grant = Grant {
            credential_id: record.id.clone(),
            environment,
            library_id: library,
            actions: vec![Action::Upload],
        };
        Ok(CurrentAuthorization { record, grant })
    })
    .transpose()
}

#[cfg(target_arch = "wasm32")]
pub(crate) async fn connect_stage_role(
    env: &worker::Env,
    binding: &str,
    role: &str,
) -> Result<Client, TrialError> {
    use crate::worker_entry::with_deadline;
    use worker::{SecureTransport, Socket};
    use TrialError::{Refused, Unconfirmed};

    with_deadline(
        async {
            let target = env.hyperdrive(binding).map_err(|_| Unconfirmed)?;
            let config: tokio_postgres::Config = target
                .connection_string()
                .parse()
                .map_err(|_| Unconfirmed)?;
            if config.get_dbname() != Some("nntunev2_staging")
                || config.get_user() != Some(role)
                || config.get_options().is_some()
            {
                return Err(Refused);
            }
            let socket = Socket::builder()
                .secure_transport(SecureTransport::StartTls)
                .connect(target.host(), target.port())
                .map_err(|_| Unconfirmed)?;
            socket.opened().await.map_err(|_| Unconfirmed)?;
            let (client, connection) = config
                .connect_raw(socket, worker::postgres_tls::PassthroughTls)
                .await
                .map_err(|_| Unconfirmed)?;
            wasm_bindgen_futures::spawn_local(async move {
                let _ = connection.await;
            });
            let identity = client
                .query_typed_one(
                    "SELECT EXISTS (SELECT FROM pg_catalog.pg_roles r
                     WHERE session_user = current_user AND r.rolname = current_user
                       AND r.rolname::text = $1 AND current_database() = 'nntunev2_staging'
                       AND r.rolcanlogin AND NOT (r.rolsuper OR r.rolbypassrls OR r.rolcreatedb
                                                 OR r.rolcreaterole OR r.rolreplication))",
                    &[(&role, Type::TEXT)],
                )
                .await
                .map_err(|_| Unconfirmed)?;
            if !identity.try_get::<_, bool>(0).map_err(|_| Unconfirmed)? {
                return Err(Refused);
            }
            Ok(client)
        },
        3_000,
        Unconfirmed,
    )
    .await
}
