use crate::handlers::{
    invalid_request, json_response, not_found, unavailable, AsyncDns, Ctx, Dialer, Mailer, Request,
    Response, SingleUseStore, Store, StoreError,
};
use crate::{
    ingest_session_caller::{
        self, Authorization, CommittedFlacUpload, CurrentAuthorization, PreparedUpload,
    },
    private_library::{stage_admission, Admission},
    private_listening::PrivateListening,
    session::{SessionClaims, PRIVILEGE_REAUTH_WINDOW_SECS},
    storage_digest_read::uuid_text,
};
use nntune_core::s3_locator::{S3Binding, S3Locator};
use nntune_ingest::{
    admin_auto_publish::{
        completed_upload::{stage_completed_release, VerifiedUpload},
        AuthorizedRelease, CommittedFlac, ExpectedFile, StageOutcome,
    },
    admin_review::ObjectRef,
    catalogue_publication::{PublicationError, PublishedObject},
    matching::admin_publish_match::{CompleteReleaseCandidates, FileNumbering},
    multipart::{session_outcome::CompletionScope, session_upload::begin},
};
use sha2::{Digest, Sha256};
use tokio_postgres::{types::Type, Client, Transaction};

const STAGING_ORIGIN: &str = "https://staging.nntune.com";
// SHA-256 of the original CC0 tone-a.flac in the private staging manifest.
pub(super) const FIXTURE_DIGEST: [u8; 32] = [
    0xc9, 0x35, 0xd3, 0x36, 0x8a, 0x1f, 0x2f, 0x22, 0xca, 0x87, 0xb8, 0xcc, 0xfa, 0x6b, 0x9f, 0xac,
    0xae, 0x07, 0x0e, 0x6e, 0x07, 0xbe, 0x1e, 0x16, 0xab, 0xff, 0x51, 0x38, 0x35, 0xa5, 0xf2, 0x59,
];

/// Selected in protected staging configuration independently of the request.
#[derive(Clone, Copy)]
pub struct Target<'a> {
    pub invite_id: i64,
    pub job_id: &'a str,
}

pub struct Permit<'a> {
    target: Target<'a>,
    policy: &'a PrivateListening,
    actor: SessionClaims,
    digest: [u8; 32],
}

#[derive(Debug, PartialEq, Eq)]
pub enum TrialError {
    Refused,
    Unconfirmed,
}
use TrialError::{Refused, Unconfirmed};

#[derive(Debug, PartialEq, Eq)]
pub struct CommittedPublication {
    track_id: String,
    digest: [u8; 32],
}

/// Mount with a separately armed staging target and trusted upload adapter.
/// The default dispatcher leaves this route off.
pub async fn upload<S, M, D, N>(
    ctx: &Ctx<'_, S, M, D, N>,
    req: &Request,
    target: Option<Target<'_>>,
    run: impl AsyncFnOnce(Permit<'_>, &[u8]) -> Result<CommittedPublication, TrialError>,
) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let Some((target, policy)) = target.zip(ctx.cfg.private_listening.as_ref()) else {
        return not_found();
    };
    if req.method != "POST"
        || req.path != super::PATH
        || ctx.cfg.origin != STAGING_ORIGIN
        || ctx.now >= policy.expires_at
        || policy.library_scope.is_none()
        || !policy.invite_ids.contains(&target.invite_id)
        || !nntune_ingest::jobs::validate_job_id(target.job_id)
    {
        return not_found();
    }
    let actor = match ctx.authenticate_admin(req, true).await {
        Ok(actor) if actor.user_id == policy.owner_user_id => actor,
        Ok(_) => return not_found(),
        Err(response) => return response,
    };
    if req.body.len() != super::FIXTURE_BYTES
        || <[u8; 32]>::from(Sha256::digest(&req.body)) != FIXTURE_DIGEST
    {
        return invalid_request();
    }
    match run(
        Permit {
            target,
            policy,
            actor,
            digest: FIXTURE_DIGEST,
        },
        &req.body,
    )
    .await
    {
        Ok(result) => {
            let digest: String = result
                .digest
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            let mut response = json_response(
                200,
                serde_json::json!({"trackId": result.track_id, "sha256": digest}),
            );
            response
                .extra_headers
                .push(("cache-control".into(), "private, no-store".into()));
            response
        }
        Err(Refused) => not_found(),
        Err(Unconfirmed) => unavailable(),
    }
}

pub struct MatchEvidence<'a> {
    pub query: &'a str,
    pub candidates: Option<CompleteReleaseCandidates<'a>>,
    pub recordings: (u16, &'a str),
    pub numbering: FileNumbering<'a>,
}

impl Permit<'_> {
    async fn recipient(&self, tx: &Transaction<'_>) -> Result<String, TrialError> {
        let authenticated = i64::try_from(self.actor.created_at).map_err(|_| Refused)?;
        let stepped_up_until = i64::try_from(
            self.actor
                .created_at
                .saturating_add(PRIVILEGE_REAUTH_WINDOW_SECS),
        )
        .map_err(|_| Refused)?;
        let expires = i64::try_from(self.actor.expires_at).map_err(|_| Refused)?;
        let rotation = i32::try_from(self.actor.rotation_counter).map_err(|_| Refused)?;
        let cohort_until = i64::try_from(self.policy.expires_at).map_err(|_| Refused)?;
        tx.query_opt(
            include_str!("lock.sql"),
            &[
                &self.actor.user_id,
                &self.actor.session_id,
                &self.target.invite_id,
                &authenticated,
                &expires,
                &rotation,
                &stepped_up_until,
                &cohort_until,
            ],
        )
        .await
        .map_err(|_| Unconfirmed)?
        .ok_or(Refused)?
        .try_get(0)
        .map_err(|_| Unconfirmed)
    }

    /// Recheck live account, exact redeemed invite and owner session before opening S3.
    pub async fn preflight(&self, publication_writer: &mut Client) -> Result<(), TrialError> {
        let tx = begin(publication_writer).await.map_err(|_| Unconfirmed)?;
        self.recipient(&tx).await?;
        tx.rollback().await.map_err(|_| Unconfirmed)
    }

    /// Publish one exact authenticated FLAC and admit it atomically. The upload writer
    /// and current authorization reader are separate from the publication connection.
    pub async fn publish(
        self,
        publication_writer: &mut Client,
        upload_writer: &mut Client,
        auth: Authorization<'_>,
        prepared: PreparedUpload<'_>,
        committed: &CommittedFlacUpload<'_, '_>,
        evidence: MatchEvidence<'_>,
        read_authority: impl AsyncFnMut(
            &str,
            &CompletionScope<'_>,
        ) -> Result<Option<CurrentAuthorization>, StoreError>,
    ) -> Result<CommittedPublication, TrialError> {
        let mut read_authority = read_authority;
        let scope = prepared.completed_scope();
        let verified = committed.verified();
        let inventory = verified.inventory();
        let pin = inventory.media();
        let digest = verified.origin().plaintext_sha256();
        if scope.principal != self.actor.user_id || digest != &self.digest {
            return Err(Refused);
        }
        let version = pin.version.ok_or(Refused)?;
        let locator = S3Locator::new(
            inventory.bucket().into(),
            pin.object_key.into(),
            S3Binding::VersionId(version.into()),
        )
        .map_err(|_| Refused)?;
        let page_versions = inventory
            .pages()
            .iter()
            .map(|(_, pin)| pin.version.ok_or(Refused))
            .collect::<Result<Vec<_>, _>>()?;
        let root = prepared.context.identities[0];
        let receipt = PublishedObject {
            intent: prepared.intent,
            locator: &locator,
            plaintext_sha256: digest,
            root_version_id: root,
            page_versions: &page_versions,
        };
        let object = ObjectRef {
            media: nntune_core::s3_locator::S3MediaMetadata::from_trusted_context(
                locator.clone(),
                prepared.context,
            )
            .ok_or(Refused)?,
            plaintext_sha256: *digest,
        };
        let release = AuthorizedRelease {
            job_id: self.target.job_id,
            owner_id: scope.principal,
            environment: scope.environment,
            library: scope.library,
            files: &[ExpectedFile {
                numbering: evidence.numbering,
                object: &object,
            }],
        };
        let mut tx = begin(publication_writer).await.map_err(|_| Unconfirmed)?;
        let recipient = self.recipient(&tx).await?;
        if upload_writer
            .query_typed_opt(
                "SELECT 1 FROM public.ingest_session_reservations
             WHERE id = $1::text::uuid AND principal_id = $2 AND idempotency_key = $3
               AND terminal_reason = 'completed' AND multipart_object_id = $4::text::uuid",
                &[
                    (&scope.session, Type::TEXT),
                    (&scope.principal, Type::TEXT),
                    (&self.target.job_id, Type::TEXT),
                    (&uuid_text(prepared.intent.identities[2]), Type::TEXT),
                ],
            )
            .await
            .map_err(|_| Unconfirmed)?
            .is_none()
        {
            return Err(Refused);
        }
        tx.execute(
            "INSERT INTO public.storage_s3_locator_intents (object_id, root_version_id, s3_bucket, s3_key, s3_version_id)
             VALUES ($1::text::uuid, $2::text::uuid, $3, $4, $5) ON CONFLICT (object_id) DO NOTHING",
            &[&uuid_text(prepared.intent.identities[2]), &uuid_text(root), &inventory.bucket(), &pin.object_key, &version],
        ).await.map_err(|_| Unconfirmed)?;
        let staged = stage_completed_release(
            &mut tx,
            &release,
            &[VerifiedUpload {
                scope,
                flac: CommittedFlac { verified, receipt },
            }],
            evidence.query,
            evidence.candidates,
            evidence.recordings,
            async |_| {
                ingest_session_caller::read(upload_writer, auth, prepared, &mut read_authority)
                    .await
                    .map_err(|error| match error {
                        ingest_session_caller::UploadError::Refused => PublicationError::Refused,
                        ingest_session_caller::UploadError::Unconfirmed => {
                            PublicationError::Indeterminate
                        }
                    })
            },
        )
        .await
        .map_err(|error| match error {
            PublicationError::Refused => Refused,
            PublicationError::Indeterminate => Unconfirmed,
        })?;
        let StageOutcome::Staged(tracks) = staged else {
            return Err(Refused);
        };
        let [track] = tracks.as_slice() else {
            return Err(Unconfirmed);
        };
        if !stage_admission(
            &tx,
            Some(self.policy),
            Admission {
                issuer: &self.actor.user_id,
                recipient: &recipient,
                invite_id: self.target.invite_id,
                track_id: track,
                job_id: self.target.job_id,
                plaintext_sha256: digest,
            },
        )
        .await
        .map_err(|_| Unconfirmed)?
        {
            return Err(Refused);
        }
        let cohort_until = i64::try_from(self.policy.expires_at).map_err(|_| Refused)?;
        if !tx
            .query_one(include_str!("reference.sql"), &[&track, &cohort_until])
            .await
            .map_err(|_| Unconfirmed)?
            .try_get::<_, bool>(0)
            .map_err(|_| Unconfirmed)?
        {
            return Err(Refused);
        }
        // Recheck the wall-clock deadlines after staging and before the commit.
        self.recipient(&tx).await?;
        tx.commit().await.map_err(|_| Unconfirmed)?;
        Ok(CommittedPublication {
            track_id: track.clone(),
            digest: *digest,
        })
    }
}
