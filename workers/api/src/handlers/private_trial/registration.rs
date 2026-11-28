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
pub(crate) struct IdentityProbeError {
    pub class: &'static str,
    pub sqlstate: Option<String>,
}

#[cfg(target_arch = "wasm32")]
impl IdentityProbeError {
    pub const fn new(class: &'static str) -> Self {
        Self {
            class,
            sqlstate: None,
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl From<tokio_postgres::Error> for IdentityProbeError {
    fn from(error: tokio_postgres::Error) -> Self {
        if let Some(code) = error.code() {
            let code = code.code();
            let sqlstate = (code.len() == 5
                && code
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()))
            .then(|| code.to_owned());
            return Self {
                class: if code.starts_with("28") {
                    "authentication"
                } else {
                    "postgres"
                },
                sqlstate,
            };
        }
        // tokio-postgres 0.7.18 exposes no authentication-kind accessor.
        // Match only its fixed Display label, which excludes the error cause.
        Self::new(if error.is_closed() {
            "connection_closed"
        } else if error.to_string() == "authentication error" {
            "authentication"
        } else {
            "driver"
        })
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) async fn connect_stage_role(
    env: &worker::Env,
    binding: &str,
    role: &str,
) -> Result<Client, TrialError> {
    connect_stage_role_with_phase(env, binding, role, &std::cell::Cell::new(""))
        .await
        .map_err(|err| match err.class {
            "database_mismatch" | "user_mismatch" => TrialError::Refused,
            _ => TrialError::Unconfirmed,
        })
}

#[cfg(target_arch = "wasm32")]
fn startup_field(
    target: &worker::Hyperdrive,
    field: &str,
    error_class: &'static str,
) -> Result<String, IdentityProbeError> {
    worker::js_sys::Reflect::get(target.as_ref(), &field.into())
        .ok()
        .and_then(|value| value.as_string())
        .filter(|value| !value.is_empty() && !value.contains('\0'))
        .ok_or_else(|| IdentityProbeError::new(error_class))
}

#[cfg(target_arch = "wasm32")]
pub(crate) async fn connect_stage_role_with_phase(
    env: &worker::Env,
    binding: &str,
    role: &str,
    phase: &std::cell::Cell<&'static str>,
) -> Result<Client, IdentityProbeError> {
    use crate::worker_entry::with_deadline;
    use tokio_postgres::{config::SslMode, Config, NoTls};

    with_deadline(
        async {
            phase.set("hyperdrive_binding");
            let target = env
                .hyperdrive(binding)
                .map_err(|_| IdentityProbeError::new("missing_or_invalid_binding"))?;
            phase.set("binding_identity");
            let database = startup_field(&target, "database", "invalid_binding_database")?;
            let user = startup_field(&target, "user", "invalid_binding_user")?;
            // These are startup fields for the Hyperdrive edge. Verify the
            // independently pinned origin identities with SQL after startup.
            let mut config = Config::new();
            config
                .dbname(database)
                .user(user)
                .password(zeroize::Zeroizing::new(target.password()).as_bytes())
                .ssl_mode(SslMode::Disable);
            phase.set("edge_connect");
            let socket = target
                .connect()
                .map_err(|_| IdentityProbeError::new("socket"))?;
            socket
                .opened()
                .await
                .map_err(|_| IdentityProbeError::new("socket"))?;
            phase.set("postgres_startup_auth");
            let (client, connection) = config.connect_raw(socket, NoTls).await?;
            wasm_bindgen_futures::spawn_local(async move {
                let _ = connection.await;
            });
            phase.set("role_query");
            let identity = client
                .query_typed_one(
                    "SELECT pg_catalog.current_database()::text = 'nntunev2_staging',
                            session_user::text = $1 AND current_user::text = $1",
                    &[(&role, Type::TEXT)],
                )
                .await?;
            if !identity.try_get::<_, bool>(0)? {
                return Err(IdentityProbeError::new("database_mismatch"));
            }
            if !identity.try_get::<_, bool>(1)? {
                return Err(IdentityProbeError::new("user_mismatch"));
            }
            Ok(client)
        },
        3_000,
        IdentityProbeError::new("deadline"),
    )
    .await
}
