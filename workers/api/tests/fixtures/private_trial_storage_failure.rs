//! Failure at multipart completion before storage can issue a version.

use super::*;
use crate::{ingest_session_auth::Job, ingest_session_request::Request as UploadRequest};
use nntune_core::{
    digest_readback::{EnrolledEnvelope, EnrolledReadbackError},
    media_envelope::EnvelopeContext,
};
use nntune_ingest::multipart::{
    initiation::InitiatedUpload,
    readback_inventory::{CompletedReadbackInventory, VerifiedCompletedReadback},
    resume_checkpoint::ResumeBinding,
    session_completion::{CompletionResponse, DispatchError, SessionCompletion},
    session_upload::{handoff, DurableUpload, HandoffError},
    sidecar::{CompletedSidecar, SidecarPut},
    CompletedVersionedMedia, MultipartError, UploadPart, UploadedPart,
};
use private_trial::{
    registered::registered_header, registration::Enrollment, sealed_fixture::TrialStorage,
    CommittedPublication, TrialError,
};
use sha2::{Digest, Sha256};
use std::{
    cell::{Cell, OnceCell},
    time::{Duration, SystemTime},
};

pub(super) const JOB: &str = "synthetic-trial-replacement";
const BUCKET: &str = "fixture-bucket";
const UPLOAD: &str = "replacement-upload-fixture";

struct RefusedCompletion {
    configuration: [u8; 16],
    key: String,
    part_checksum: OnceCell<String>,
    completions: Cell<u8>,
}

impl TrialStorage for RefusedCompletion {
    fn matches_storage(&self, configuration: [u8; 16], bucket: &str) -> bool {
        (configuration, bucket) == (self.configuration, BUCKET)
    }

    async fn open_upload<'a>(
        &'a self,
        db: &mut Client,
        scope: &CompletionScope<'_>,
        lease: &str,
        context: &'a EnvelopeContext,
        key: &'a str,
        until: SystemTime,
    ) -> Result<DurableUpload<'a>, HandoffError> {
        assert_eq!(key, self.key);
        handoff(db, scope, lease, context, (BUCKET, key), until, async |_| {
            let response = format!(
                "<InitiateMultipartUploadResult><Bucket>{BUCKET}</Bucket><Key>{key}</Key><UploadId>{UPLOAD}</UploadId></InitiateMultipartUploadResult>"
            );
            Ok(InitiatedUpload::verify(self.configuration, BUCKET, key, 200, response.as_bytes()).unwrap())
        }).await
    }

    async fn put_part<'a>(
        &self,
        _: ResumeBinding<'a>,
        _: &EnvelopeContext,
        part: &UploadPart<'a>,
        upload: &DurableUpload<'_>,
        _: SystemTime,
    ) -> Result<UploadedPart<'a>, DispatchError> {
        let (target, number, _, checksum) = part.request();
        assert!(target == upload.target());
        assert_eq!(number, 1);
        self.part_checksum.set(checksum.to_owned()).unwrap();
        Ok(part
            .verify_reply("\"replacement-part\"".into(), checksum)
            .unwrap())
    }

    async fn complete(
        &self,
        completion: SessionCompletion<'_>,
        _: SystemTime,
    ) -> Result<CompletedVersionedMedia, DispatchError> {
        let result = completion
            .dispatch(async |sent| {
                self.completions.set(self.completions.get() + 1);
                assert_eq!(
                    (sent.target.bucket, sent.target.key, sent.target.upload_id),
                    (BUCKET, self.key.as_str(), UPLOAD)
                );
                let checksum = self
                    .part_checksum
                    .get()
                    .expect("the replacement part was uploaded");
                assert!(std::str::from_utf8(&sent.body)
                    .unwrap()
                    .contains(&format!("<ChecksumSHA256>{checksum}</ChecksumSHA256>")));
                Ok(CompletionResponse {
                    configuration: self.configuration,
                    status: 503,
                    body: Vec::new(),
                    version: None,
                })
            })
            .await;
        assert!(matches!(
            &result,
            Err(DispatchError::Unconfirmed(Some(
                MultipartError::CompletionStatus
            )))
        ));
        result
    }

    async fn confirm_head(
        &self,
        _: &CompletedVersionedMedia,
        _: &EnvelopeContext,
        _: SystemTime,
    ) -> Result<(), ()> {
        panic!("failed completion reached HEAD")
    }

    async fn put_sidecar<'a>(
        &self,
        _: SidecarPut<'a>,
        _: SystemTime,
    ) -> Result<CompletedSidecar<'a>, ()> {
        panic!("failed completion reached sidecar upload")
    }

    async fn verify_readback<'i, 'r>(
        &self,
        _: &'i CompletedReadbackInventory<'r>,
        _: EnrolledEnvelope<'_>,
        _: SystemTime,
    ) -> Result<VerifiedCompletedReadback<'i, 'r>, EnrolledReadbackError> {
        panic!("failed completion reached authenticated readback")
    }
}

pub(super) async fn attempt(
    observer: &Client,
    writer: &mut Client,
    body: &[u8],
    auth: Authorization<'_>,
    prior: PreparedUpload<'_>,
) -> Result<CommittedPublication, TrialError> {
    // Release the prior upload's digest reservation while keeping its validated
    // winner and playback reference. The trial accepts only this fixture FLAC.
    let prior_object = uuid_text(prior.intent.identities[2]);
    let released = observer
        .query_one(
            "SELECT public.release_storage_digest($1::text::uuid, $2)",
            &[&prior_object, &prior.intent.reservation_generation],
        )
        .await
        .unwrap();
    assert!(released.get::<_, bool>(0));
    observer.execute(
        "INSERT INTO public.ingest_jobs (id,user_id,idempotency_key,accepted_at,status) VALUES ($1,$2,$1,1,'done')",
        &[&JOB, &prior.scope.principal],
    ).await.unwrap();

    let [environment, library, _, _, configuration] = prior.intent.identities;
    let [session, object, generation, map] = [0xf4, 0xf5, 0xf6, 0xf7].map(|value| {
        let mut id = [value; 16];
        id[6] = 0x40;
        id[8] = 0x80;
        id
    });
    let [session, obj, seal] = [session, object, generation].map(uuid_text);
    let request = UploadRequest {
        file_bytes: body.len() as u64,
        content_sha256: Sha256::digest(body).into(),
        title: "Synthetic trial tone",
        artist: "NNTune",
        album: Some(super::QUERY),
    };
    let fingerprint = request.fingerprint().unwrap();
    let identities = [environment, library, object, generation, configuration];
    let mut registrar = connection().await;
    let fixture = Enrollment {
        session: &session,
        job: JOB,
        actor: prior.scope.principal,
        fingerprint: fingerprint.bytes(),
    }
    .register(
        &mut registrar,
        body,
        identities,
        map,
        prior.context.identities[0],
        &[17; 32],
    )
    .await
    .expect("the fresh replacement must register before storage fails");
    let job = Job {
        id: &session,
        ..*prior.job
    };
    let prepared = PreparedUpload {
        scope: CompletionScope {
            session: &session,
            seal: generation,
            fingerprint: fingerprint.bytes(),
            ..prior.scope
        },
        job: &job,
        request: &request,
        context: &fixture.context,
        intent: &fixture.intent,
    };
    let storage = RefusedCompletion {
        configuration,
        key: format!("media/{obj}/{seal}"),
        part_checksum: OnceCell::new(),
        completions: Cell::new(0),
    };
    let failed = fixture
        .send(
            writer,
            auth,
            prepared,
            &storage,
            BUCKET,
            &[17; 32],
            SystemTime::now() + Duration::from_secs(30),
            auth.record.expires_at as i64,
            async |_, _| panic!("failed completion reached authority reload"),
            async |_, _| panic!("failed completion reached publication"),
        )
        .await;
    assert_eq!(storage.completions.get(), 1);

    let row = observer.query_one(
        "SELECT multipart_upload_id = $2 AND multipart_object_id = $3::text::uuid
             AND terminal_reason IS NULL AND completed_map_id IS NULL
             AND NOT EXISTS (SELECT FROM public.storage_digest_inventory_intents WHERE map_id = $4::text::uuid)
             AND NOT EXISTS (SELECT FROM public.storage_validated_digest_winners WHERE object_id = $3::text::uuid)
             AND NOT EXISTS (SELECT FROM public.storage_s3_locator_intents WHERE object_id = $3::text::uuid)
             AND NOT EXISTS (SELECT FROM public.catalogue_s3_sources WHERE source_job_id = $5 OR object_id = $3::text::uuid)
             AND NOT EXISTS (SELECT FROM public.private_library_admissions WHERE object_id = $3::text::uuid)
             AND NOT EXISTS (SELECT FROM public.storage_validated_digest_references WHERE object_id = $3::text::uuid)
         FROM public.ingest_session_reservations WHERE id = $1::text::uuid",
        &[&session, &UPLOAD, &obj, &uuid_text(map), &JOB],
    ).await.unwrap();
    assert!(row.get::<_, bool>(0));
    // A registered incomplete upload reaches the same refusal that blocks the
    // Worker's retry before it can open another upload or replay a receipt.
    assert_eq!(
        registered_header(writer, &prepared.scope, identities, map, JOB).await,
        Err(TrialError::Unconfirmed)
    );
    failed
}
