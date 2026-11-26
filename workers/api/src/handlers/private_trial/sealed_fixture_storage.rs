//! Provider operations used by the sealed trial sender.

use std::time::SystemTime;

use nntune_core::{
    digest_readback::{EnrolledEnvelope, EnrolledReadbackError},
    media_envelope::EnvelopeContext,
};
use nntune_ingest::multipart::{
    readback_inventory::{CompletedReadbackInventory, VerifiedCompletedReadback},
    resume_checkpoint::ResumeBinding,
    session_completion::{DispatchError, SessionCompletion},
    session_outcome::CompletionScope,
    session_upload::{DurableUpload, HandoffError},
    sidecar::{CompletedSidecar, SidecarPut},
    CompletedVersionedMedia, UploadPart, UploadedPart,
};
use tokio_postgres::Client;

pub(crate) trait TrialStorage {
    fn matches_storage(&self, configuration: [u8; 16], bucket: &str) -> bool;
    async fn open_upload<'a>(
        &'a self,
        db: &mut Client,
        scope: &CompletionScope<'_>,
        lease: &str,
        context: &'a EnvelopeContext,
        key: &'a str,
        until: SystemTime,
    ) -> Result<DurableUpload<'a>, HandoffError>;
    async fn put_part<'a>(
        &self,
        binding: ResumeBinding<'a>,
        context: &EnvelopeContext,
        part: &UploadPart<'a>,
        upload: &DurableUpload<'_>,
        until: SystemTime,
    ) -> Result<UploadedPart<'a>, DispatchError>;
    async fn complete(
        &self,
        upload: &DurableUpload<'_>,
        completion: SessionCompletion<'_>,
    ) -> Result<CompletedVersionedMedia, DispatchError>;
    async fn confirm_head(
        &self,
        media: &CompletedVersionedMedia,
        context: &EnvelopeContext,
        until: SystemTime,
    ) -> Result<(), ()>;
    async fn put_sidecar<'a>(
        &self,
        put: SidecarPut<'a>,
        until: SystemTime,
    ) -> Result<CompletedSidecar<'a>, ()>;
    async fn verify_readback<'i, 'r>(
        &self,
        inventory: &'i CompletedReadbackInventory<'r>,
        envelope: EnrolledEnvelope<'_>,
        until: SystemTime,
    ) -> Result<VerifiedCompletedReadback<'i, 'r>, EnrolledReadbackError>;
}

#[cfg(target_arch = "wasm32")]
use nntune_ingest::multipart::s3_completion::S3CompletionSender;

#[cfg(target_arch = "wasm32")]
impl TrialStorage for S3CompletionSender {
    fn matches_storage(&self, configuration: [u8; 16], bucket: &str) -> bool {
        self.matches_storage(configuration, bucket)
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
        self.open_upload(db, scope, lease, context, key, until)
            .await
    }
    async fn put_part<'a>(
        &self,
        binding: ResumeBinding<'a>,
        context: &EnvelopeContext,
        part: &UploadPart<'a>,
        upload: &DurableUpload<'_>,
        until: SystemTime,
    ) -> Result<UploadedPart<'a>, DispatchError> {
        self.put_part(binding, context, part, upload, until).await
    }
    async fn complete(
        &self,
        upload: &DurableUpload<'_>,
        completion: SessionCompletion<'_>,
    ) -> Result<CompletedVersionedMedia, DispatchError> {
        self.complete(upload, completion).await
    }
    async fn confirm_head(
        &self,
        media: &CompletedVersionedMedia,
        context: &EnvelopeContext,
        until: SystemTime,
    ) -> Result<(), ()> {
        self.confirm_head(media, context, until)
            .await
            .map_err(|_| ())
    }
    async fn put_sidecar<'a>(
        &self,
        put: SidecarPut<'a>,
        until: SystemTime,
    ) -> Result<CompletedSidecar<'a>, ()> {
        self.put_sidecar(put, until).await.map_err(|_| ())
    }
    async fn verify_readback<'i, 'r>(
        &self,
        inventory: &'i CompletedReadbackInventory<'r>,
        envelope: EnrolledEnvelope<'_>,
        until: SystemTime,
    ) -> Result<VerifiedCompletedReadback<'i, 'r>, EnrolledReadbackError> {
        self.verify_readback(inventory, envelope, until).await
    }
}
