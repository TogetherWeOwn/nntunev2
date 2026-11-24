//! Worker-only adapter for the synthetic trial target.

use std::time::{Duration, UNIX_EPOCH};

use nntune_core::media_envelope::EnvelopeContext;
use nntune_ingest::{
    digest_candidate_store::CandidateIntent,
    matching::{
        admin_publish_match::{release_lookup, CompleteReleaseCandidates, FileNumbering},
        MbSearchHit,
    },
    multipart::{s3_completion::S3CompletionSender, session_outcome::CompletionScope},
};
use serde::Deserialize;
use tokio_postgres::{types::Type, Client};
use worker::{Env, SecureTransport, Socket};
use zeroize::Zeroizing;

use crate::{
    ingest_session_auth::Job,
    ingest_session_caller::{Authorization, PreparedUpload},
    ingest_session_request::Request,
    native_rest::Pepper,
    storage_digest_read::uuid_text,
    worker_entry::with_deadline,
};

use super::super::{
    read_registered_header,
    registration::{load_authority, Enrollment},
    FIXTURE_BYTES,
};
use super::{
    CommittedPublication, MatchEvidence, Permit, Target,
    TrialError::{self, Refused, Unconfirmed},
};

const BUCKET: &str = "nntunev2-private-fixtures-staging";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Selection {
    invite_id: i64,
    job_id: String,
    session_id: String,
    object_id: String,
    generation_id: String,
    map_id: String,
    credential_id: String,
    title: String,
    release_mbid: String,
}

fn var(env: &Env, name: &str) -> Option<String> {
    Some(env.var(name).ok()?.to_string())
}

fn secret(env: &Env, name: &str) -> Result<Zeroizing<String>, TrialError> {
    env.secret(name)
        .map(|value| Zeroizing::new(value.to_string()))
        .map_err(|_| Unconfirmed)
}

fn uuid(raw: &str) -> Option<[u8; 16]> {
    if !crate::meter_sink::is_uuid_shaped(raw) {
        return None;
    }
    let id = decode_hex(&raw.replace('-', ""))?;
    (id[6] >> 4 == 4 && id[8] >> 6 == 2).then_some(id)
}

fn decode_hex<const N: usize>(raw: &str) -> Option<[u8; N]> {
    if raw.len() != N * 2
        || !raw
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let mut result = [0; N];
    for (i, byte) in result.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&raw[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(result)
}

pub(crate) fn selected(env: &Env) -> Option<Selection> {
    if var(env, "SUBSONIC_MEASUREMENT_STAGE").as_deref() != Some("staging")
        || var(env, "PRIVATE_TRIAL_UPLOAD_ENABLED").as_deref() != Some("true")
        || var(env, "PRIVATE_PLAYBACK_ENABLED").as_deref() != Some("true")
    {
        return None;
    }
    let selection = secret(env, "PRIVATE_TRIAL_UPLOAD_TARGET").ok()?;
    if selection.len() > 2_048 {
        return None;
    }
    let selected: Selection = serde_json::from_str(&selection).ok()?;
    let ids = [
        &selected.session_id,
        &selected.object_id,
        &selected.generation_id,
        &selected.map_id,
    ];
    if selected.invite_id <= 0
        || ids.into_iter().any(|id| uuid(id).is_none())
        || selected.title.len() > 128
        || selected.credential_id.is_empty()
        || selected.credential_id.len() > 64
        || !crate::meter_sink::is_uuid_shaped(&selected.release_mbid)
    {
        return None;
    }
    Some(selected)
}

impl Selection {
    pub(crate) fn target(&self) -> Target<'_> {
        Target {
            invite_id: self.invite_id,
            job_id: &self.job_id,
        }
    }

    pub(crate) async fn run(
        &self,
        env: &Env,
        permit: Permit<'_>,
        body: &[u8],
    ) -> Result<CommittedPublication, TrialError> {
        let until = UNIX_EPOCH
            .checked_add(Duration::from_secs(
                permit.policy.expires_at.min(permit.actor.expires_at),
            ))
            .ok_or(Refused)?;
        let retain = i64::try_from(permit.policy.expires_at).map_err(|_| Refused)?;
        let scope = |name| uuid(&var(env, name)?);
        let environment = scope("PRIVATE_PLAYBACK_ENVIRONMENT").ok_or(Unconfirmed)?;
        let library = scope("PRIVATE_PLAYBACK_LIBRARY").ok_or(Unconfirmed)?;
        let configuration = scope("PRIVATE_PLAYBACK_S3_CONFIGURATION").ok_or(Unconfirmed)?;
        let root_version = scope("PRIVATE_PLAYBACK_ROOT_VERSION").ok_or(Unconfirmed)?;
        if var(env, "PRIVATE_PLAYBACK_S3_BUCKET").as_deref() != Some(BUCKET)
            || permit
                .policy
                .library_scope
                .as_ref()
                .is_none_or(|scope| !scope.matches(environment, library))
        {
            return Err(Refused);
        }
        let root = secret(env, "PRIVATE_PLAYBACK_ROOT_KEY")?;
        let root = Zeroizing::new(decode_hex::<32>(&root).ok_or(Unconfirmed)?);
        let bearer = secret(env, "PRIVATE_TRIAL_UPLOAD_BEARER")?;
        let pepper = secret(env, "PRIVATE_TRIAL_UPLOAD_PEPPER")?;
        let version = var(env, "PRIVATE_TRIAL_UPLOAD_PEPPER_VERSION")
            .and_then(|number| number.parse().ok())
            .ok_or(Unconfirmed)?;
        let peppers = [Pepper {
            version,
            bytes: pepper.as_bytes(),
        }];
        let sender = S3CompletionSender::new(
            configuration,
            BUCKET.into(),
            &var(env, "PRIVATE_TRIAL_S3_ENDPOINT").ok_or(Unconfirmed)?,
            var(env, "PRIVATE_TRIAL_S3_REGION").ok_or(Unconfirmed)?,
            secret(env, "PRIVATE_TRIAL_S3_ACCESS_KEY_ID")?.to_string(),
            secret(env, "PRIVATE_TRIAL_S3_SECRET_ACCESS_KEY")?.to_string(),
        )
        .map_err(|_| Unconfirmed)?;

        let bindings = [
            "PRIVATE_TRIAL_UPLOAD_DB",
            "PRIVATE_TRIAL_AUTH_DB",
            "PRIVATE_TRIAL_PUBLISH_DB",
            "PRIVATE_TRIAL_ENROLL_DB",
        ];
        let roles = bindings
            .iter()
            .map(|name| var(env, &format!("{name}_ROLE")).ok_or(Unconfirmed))
            .collect::<Result<Vec<_>, _>>()?;
        if roles.iter().collect::<std::collections::HashSet<_>>().len() != roles.len() {
            return Err(Refused);
        }
        let mut writer = connect(env, bindings[0], &roles[0]).await?;
        let reader = connect(env, bindings[1], &roles[1]).await?;
        let mut publisher = connect(env, bindings[2], &roles[2]).await?;
        permit.preflight(&mut publisher).await?;

        let attempt = uuid(self.session_id.as_str()).ok_or(Refused)?;
        let session = uuid_text(attempt);
        let object = uuid(&self.object_id).ok_or(Refused)?;
        let generation = uuid(&self.generation_id).ok_or(Refused)?;
        let map = uuid(&self.map_id).ok_or(Refused)?;
        let request = Request {
            file_bytes: FIXTURE_BYTES as u64,
            content_sha256: permit.digest,
            title: "Synthetic trial tone",
            artist: "NNTune",
            album: Some(&self.title),
        };
        let fingerprint = request.fingerprint().map_err(|_| Refused)?;
        let actor = permit.actor.user_id.clone();
        let actor = actor.as_str();
        let upload_scope = CompletionScope {
            environment,
            library,
            principal: actor,
            session: &session,
            fingerprint: fingerprint.bytes(),
            seal: generation,
            revision: 0,
            offset: 0,
        };
        let job = Job {
            id: &session,
            owner_id: actor,
            environment: &uuid_text(environment),
            library_id: &uuid_text(library),
        };

        let identities = [environment, library, object, generation, configuration];
        if let Some(header) =
            read_registered_header(&reader, &upload_scope, identities, map).await?
        {
            let current = load_authority(&reader, &self.credential_id, &upload_scope)
                .await
                .map_err(|_| Unconfirmed)?
                .ok_or(Refused)?;
            let auth = Authorization {
                record: &current.record,
                grant: &current.grant,
                bearer: &bearer,
                peppers: &peppers,
            };
            let intent = CandidateIntent {
                identities,
                plaintext_len: FIXTURE_BYTES as u64,
                map_id: map,
                header,
                reservation_generation: 1,
            };
            let context = EnvelopeContext {
                identities: [
                    root_version,
                    environment,
                    library,
                    object,
                    generation,
                    configuration,
                ],
                plaintext_len: intent.plaintext_len,
                header,
            };
            let prepared = PreparedUpload {
                scope: upload_scope,
                job: &job,
                request: &request,
                context: &context,
                intent: &intent,
            };
            return permit
                .replay(
                    &mut publisher,
                    &mut writer,
                    BUCKET,
                    auth,
                    prepared,
                    async |id, scope| load_authority(&reader, id, scope).await,
                )
                .await;
        }

        // Exact-title evidence is enrolled from the verified CC0 mirror receipt.
        // The Worker does not fall back to public MusicBrainz search.
        let verified = secret(env, "PRIVATE_TRIAL_MIRROR_EVIDENCE")?;
        if verified.len() > 16_384 {
            return Err(Unconfirmed);
        }
        let result: MirrorEvidence = serde_json::from_str(&verified).map_err(|_| Unconfirmed)?;
        if result.release_mbid != self.release_mbid || result.title != self.title {
            return Err(Refused);
        }
        let lookup = release_lookup(&self.title).map_err(|_| Refused)?;
        let recordings = serde_json::json!({
            "release_mbid": result.release_mbid, "recordings": result.recordings
        })
        .to_string();
        let evidence = MatchEvidence {
            query: &self.title,
            candidates: Some(CompleteReleaseCandidates {
                lookup: &lookup,
                hits: result.exact_title_candidates,
            }),
            recordings: (200, &recordings),
            numbering: FileNumbering {
                disc: Some("1"),
                track: Some("1"),
            },
        };
        // Enrollment is isolated from both the session grant read and upload writes.
        let mut registrar = connect(env, bindings[3], &roles[3]).await?;
        let enrollment = Enrollment {
            session: &session,
            job: &self.job_id,
            actor,
            fingerprint: fingerprint.bytes(),
        };
        let fixture = enrollment
            .register(&mut registrar, body, identities, map, root_version, &root)
            .await?;
        let current = load_authority(&reader, &self.credential_id, &upload_scope)
            .await
            .map_err(|_| Unconfirmed)?
            .ok_or(Refused)?;
        let auth = Authorization {
            record: &current.record,
            grant: &current.grant,
            bearer: &bearer,
            peppers: &peppers,
        };
        let prepared = PreparedUpload {
            scope: upload_scope,
            job: &job,
            request: &request,
            context: &fixture.context,
            intent: &fixture.intent,
        };
        let reload =
            async |id: &str, scope: &CompletionScope<'_>| load_authority(&reader, id, scope).await;
        fixture
            .send(
                &mut writer,
                auth,
                prepared,
                &sender,
                BUCKET,
                &root,
                until,
                retain,
                reload,
                async |writer, committed| {
                    permit
                        .publish(
                            &mut publisher,
                            writer,
                            auth,
                            prepared,
                            committed,
                            evidence,
                            reload,
                        )
                        .await
                },
            )
            .await
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MirrorEvidence {
    release_mbid: String,
    title: String,
    exact_title_candidates: Vec<MbSearchHit>,
    recordings: Vec<serde_json::Value>,
}

async fn connect(env: &Env, binding: &str, role: &str) -> Result<Client, TrialError> {
    with_deadline(
        async {
            let target = env.hyperdrive(binding).map_err(|_| Unconfirmed)?;
            let config: tokio_postgres::Config = target
                .connection_string()
                .parse()
                .map_err(|_| Unconfirmed)?;
            if config.get_dbname() != Some("nntunev2_staging") || config.get_user() != Some(role) {
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
                    "SELECT current_user::text = $1 AND current_database() = 'nntunev2_staging'",
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
