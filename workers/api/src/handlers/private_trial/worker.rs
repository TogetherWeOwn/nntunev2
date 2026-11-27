//! Worker-only adapter for the synthetic trial target.

use nntune_core::media_envelope::EnvelopeContext;
use nntune_ingest::{
    digest_candidate_store::CandidateIntent,
    matching::admin_publish_match::{release_lookup, CompleteReleaseCandidates, FileNumbering},
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
    worker_entry::{
        now_millis,
        private_browser_worker::{hex, uuid_v4},
    },
};

use super::super::{
    parse_object,
    registered::registered_header,
    registration::{authorize_enrollment, connect_stage_role, load_authority, Enrollment},
    validate_mirror_evidence, MirrorEvidence, FIXTURE_BUCKET as BUCKET, FIXTURE_BYTES,
};
use super::{
    CommittedPublication, MatchEvidence, Permit, Target,
    TrialError::{self, Refused, Unconfirmed},
};

const NUMBERING: FileNumbering<'_> = FileNumbering {
    disc: Some("1"),
    track: Some("1"),
};

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

pub(crate) fn selected(env: &Env) -> Option<Selection> {
    if var(env, "SUBSONIC_MEASUREMENT_STAGE").as_deref() != Some("staging")
        || var(env, "PRIVATE_TRIAL_UPLOAD_ENABLED").as_deref() != Some("true")
        || var(env, "PRIVATE_PLAYBACK_ENABLED").as_deref() != Some("true")
    {
        return None;
    }
    let raw = secret(env, "PRIVATE_TRIAL_UPLOAD_TARGET").ok()?;
    parse_object::<Selection>(&raw, 2_048)
        .ok()
        .filter(|target| {
            target.invite_id > 0
                && nntune_ingest::jobs::validate_job_id(&target.job_id)
                && (1..=64).contains(&target.credential_id.len())
                && !target.credential_id.chars().any(char::is_control)
        })
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
        if self.invite_id != permit.target.invite_id || self.job_id != permit.target.job_id {
            return Err(Refused);
        }
        let session = uuid_text(uuid_v4(&self.session_id).ok_or(Refused)?);
        let object = uuid_v4(&self.object_id).ok_or(Refused)?;
        let generation = uuid_v4(&self.generation_id).ok_or(Refused)?;
        let map = uuid_v4(&self.map_id).ok_or(Refused)?;
        let lookup = release_lookup(&self.title).map_err(|_| Refused)?;
        if !nntune_ingest::queue::validate_mbid(&self.release_mbid) {
            return Err(Refused);
        }
        let step_up_end = permit
            .actor
            .created_at
            .saturating_add(crate::session::PRIVILEGE_REAUTH_WINDOW_SECS + 1);
        let cutoff = permit
            .policy
            .expires_at
            .min(permit.actor.expires_at)
            .min(step_up_end);
        if cutoff <= now_millis() / 1_000 {
            return Err(Refused);
        }
        let until = UNIX_EPOCH
            .checked_add(Duration::from_secs(cutoff))
            .ok_or(Refused)?;
        let retain = i64::try_from(permit.policy.expires_at).map_err(|_| Refused)?;
        let scope = |name| uuid_v4(&var(env, name)?);
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
        let request = Request {
            file_bytes: FIXTURE_BYTES as u64,
            content_sha256: permit.digest,
            title: "Synthetic trial tone",
            artist: "NNTune",
            album: Some(&self.title),
        };
        let fingerprint = request.fingerprint().map_err(|_| Refused)?;
        let mut writer = connect_stage_role(env, bindings[0], &roles[0]).await?;
        let reader = connect_stage_role(env, bindings[1], &roles[1]).await?;
        let mut publisher = connect_stage_role(env, bindings[2], &roles[2]).await?;
        permit.preflight(&mut publisher).await?;
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
            let recordings = serde_json::json!({
                "release_mbid": mirror.release_mbid, "recordings": mirror.recordings
            })
            .to_string();
            validate_mirror_evidence(
                &self.title,
                &self.release_mbid,
                CompleteReleaseCandidates {
                    lookup: &lookup,
                    hits: mirror.exact_title_candidates.clone(),
                },
                &recordings,
                NUMBERING,
            )?;
            let mut registrar = connect_stage_role(env, bindings[3], &roles[3]).await?;
            if cutoff <= now_millis() / 1_000 {
                return Err(Refused);
            }
            let enrollment = Enrollment {
                session: &session,
                job: &self.job_id,
                actor,
                fingerprint: fingerprint.bytes(),
            };
            let fixture = enrollment
                .register(&mut registrar, body, identities, map, root_version, &root)
                .await?;
            (
                fixture.context.header,
                Some((fixture, mirror.exact_title_candidates, recordings)),
            )
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
                .map_or(1, |(f, ..)| f.intent.reservation_generation),
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
        let Some((fixture, hits, recordings)) = sealed else {
            return permit
                .replay(&mut publisher, &mut writer, BUCKET, auth, prepared, reload)
                .await;
        };
        let evidence = MatchEvidence {
            query: &self.title,
            candidates: Some(CompleteReleaseCandidates {
                lookup: &lookup,
                hits,
            }),
            recordings: (200, &recordings),
            numbering: NUMBERING,
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
        let result: MirrorEvidence = parse_object(&verified, 16_384).map_err(|_| Unconfirmed)?;
        if result.release_mbid != self.release_mbid || result.title != self.title {
            return Err(Refused);
        }
        Ok(result)
    }
}
