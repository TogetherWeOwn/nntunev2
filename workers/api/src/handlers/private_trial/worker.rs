//! Worker-only adapter for the synthetic trial target.

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
use std::time::{Duration, UNIX_EPOCH};
use worker::Env;
use zeroize::Zeroizing;

use crate::{
    ingest_session_auth::Job,
    ingest_session_caller::{Authorization, PreparedUpload},
    ingest_session_request::Request,
    native_rest::Pepper,
    storage_digest_read::uuid_text,
    worker_entry::{now_millis, private_browser_worker::hex},
};

use super::super::{
    registered::registered_header,
    registration::{authorize_enrollment, connect_stage_role, load_authority, Enrollment},
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
    let id = hex(&raw.replace('-', ""))?;
    (id[6] >> 4 == 4 && id[8] >> 6 == 2).then_some(id)
}

pub(crate) fn selected(env: &Env) -> Option<Selection> {
    if var(env, "SUBSONIC_MEASUREMENT_STAGE").as_deref() != Some("staging")
        || var(env, "PRIVATE_TRIAL_UPLOAD_ENABLED").as_deref() != Some("true")
        || var(env, "PRIVATE_PLAYBACK_ENABLED").as_deref() != Some("true")
    {
        return None;
    }
    let raw = secret(env, "PRIVATE_TRIAL_UPLOAD_TARGET")
        .ok()
        .filter(|value| value.len() <= 2_048)?;
    serde_json::from_str::<Selection>(&raw)
        .ok()
        .filter(|target| (1..=64).contains(&target.credential_id.len()))
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
        let step_up_end = permit
            .actor
            .created_at
            .saturating_add(crate::session::PRIVILEGE_REAUTH_WINDOW_SECS + 1);
        let cutoff = permit
            .policy
            .expires_at
            .min(permit.actor.expires_at)
            .min(step_up_end);
        let until = UNIX_EPOCH
            .checked_add(Duration::from_secs(cutoff))
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
        let root = Zeroizing::new(hex::<32>(&root).ok_or(Unconfirmed)?);
        let bearer = secret(env, "PRIVATE_TRIAL_UPLOAD_BEARER")?;
        let pepper = secret(env, "PRIVATE_TRIAL_UPLOAD_PEPPER")?;
        let version = secret(env, "PRIVATE_TRIAL_UPLOAD_PEPPER_VERSION")?
            .parse()
            .ok()
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
            .map(|name| secret(env, &format!("{name}_ROLE")))
            .collect::<Result<Vec<_>, _>>()?;
        if roles
            .iter()
            .map(|r| r.as_str())
            .collect::<std::collections::HashSet<_>>()
            .len()
            != roles.len()
        {
            return Err(Refused);
        }
        let mut writer = connect_stage_role(env, bindings[0], &roles[0]).await?;
        let reader = connect_stage_role(env, bindings[1], &roles[1]).await?;
        let mut publisher = connect_stage_role(env, bindings[2], &roles[2]).await?;
        permit.preflight(&mut publisher).await?;
        let session = uuid_text(uuid(&self.session_id).ok_or(Refused)?);
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
        authorize_enrollment(
            &reader,
            &self.credential_id,
            &upload_scope,
            &bearer,
            &peppers,
            now_millis() / 1000,
        )
        .await?;
        let (header, sealed) = if let Some(header) =
            registered_header(&writer, &upload_scope, identities, map, &self.job_id).await?
        {
            (header, None)
        } else {
            let mirror = self.mirror(env)?;
            let mut registrar = connect_stage_role(env, bindings[3], &roles[3]).await?;
            let enrollment = Enrollment {
                session: &session,
                job: &self.job_id,
                actor,
                fingerprint: fingerprint.bytes(),
            };
            let fixture = enrollment
                .register(&mut registrar, body, identities, map, root_version, &root)
                .await?;
            (fixture.context.header, Some((fixture, mirror)))
        };
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
        let context = EnvelopeContext {
            identities: [
                root_version,
                environment,
                library,
                object,
                generation,
                configuration,
            ],
            plaintext_len: FIXTURE_BYTES as u64,
            header,
        };
        let intent = CandidateIntent {
            identities,
            plaintext_len: context.plaintext_len,
            map_id: map,
            header,
            reservation_generation: sealed
                .as_ref()
                .map_or(1, |(f, _)| f.intent.reservation_generation),
        };
        let prepared = PreparedUpload {
            scope: upload_scope,
            job: &job,
            request: &request,
            context: &context,
            intent: &intent,
        };
        let reload =
            async |id: &str, scope: &CompletionScope<'_>| load_authority(&reader, id, scope).await;
        let Some((fixture, mirror)) = sealed else {
            return permit
                .replay(&mut publisher, &mut writer, BUCKET, auth, prepared, reload)
                .await;
        };
        let lookup = release_lookup(&self.title).map_err(|_| Refused)?;
        let recordings = serde_json::json!({
            "release_mbid": mirror.release_mbid, "recordings": mirror.recordings
        })
        .to_string();
        let evidence = MatchEvidence {
            query: &self.title,
            candidates: Some(CompleteReleaseCandidates {
                lookup: &lookup,
                hits: mirror.exact_title_candidates,
            }),
            recordings: (200, &recordings),
            numbering: FileNumbering {
                disc: Some("1"),
                track: Some("1"),
            },
        };
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
    fn mirror(&self, env: &Env) -> Result<MirrorEvidence, TrialError> {
        env.hyperdrive("MB_DB").map_err(|_| Unconfirmed)?;
        let verified = secret(env, "PRIVATE_TRIAL_MIRROR_EVIDENCE")?;
        if verified.len() > 16_384 {
            return Err(Unconfirmed);
        }
        let result: MirrorEvidence = serde_json::from_str(&verified).map_err(|_| Unconfirmed)?;
        if result.release_mbid != self.release_mbid || result.title != self.title {
            return Err(Refused);
        }
        Ok(result)
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
