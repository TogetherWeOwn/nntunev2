//! Off-route FLAC handoff for the encrypted upload producer.

use std::ops::Range;

use nntune_core::s3_locator::S3MediaMetadata;

use super::{CommittedOutcome, VerifiedCompletedReadback};

#[cfg(any(target_arch = "wasm32", all(test, feature = "postgres-tests")))]
use {
    super::{
        Authorization, CompletionScope, CurrentAuthorization, OpenedUpload, Refused, StoreError,
        Unconfirmed, UploadError,
    },
    nntune_core::digest_readback::EnrolledReadbackError,
    nntune_ingest::multipart::readback_inventory::CompletedReadbackInventory,
    std::time::SystemTime,
    tokio_postgres::Client,
};
#[cfg(target_arch = "wasm32")]
use {
    nntune_core::digest_readback::EnrolledEnvelope,
    nntune_ingest::multipart::s3_completion::S3CompletionSender,
};

/// Indexed, authenticated FLAC bound to the exact scoped completion result.
/// The index is in memory. Consumers still establish read authority and custody.
pub struct CommittedFlacUpload<'inventory, 'receipts> {
    outcome: CommittedOutcome,
    verified: VerifiedCompletedReadback<'inventory, 'receipts>,
}

impl CommittedFlacUpload<'_, '_> {
    pub fn outcome(&self) -> &CommittedOutcome {
        &self.outcome
    }

    pub fn flac_plaintext_range(
        &self,
        media: &S3MediaMetadata,
        plaintext_sha256: &[u8; 32],
        millis: Range<u64>,
    ) -> Option<Range<u64>> {
        self.verified
            .flac_plaintext_range(media, plaintext_sha256, millis)
    }
}

#[cfg(any(target_arch = "wasm32", all(test, feature = "postgres-tests")))]
impl OpenedUpload<'_> {
    /// Read the immutable ciphertext and sidecars reported by the completed
    /// upload, then use the existing writer and fenced result reader. Use the
    /// exact completion receipts and independently trusted envelope as input.
    #[cfg(target_arch = "wasm32")]
    pub async fn commit_signed_flac<'inventory, 'receipts: 'inventory>(
        self,
        writer: &mut Client,
        auth: Authorization<'_>,
        readback: (
            &'inventory CompletedReadbackInventory<'receipts>,
            EnrolledEnvelope<'_>,
        ),
        sender: &S3CompletionSender,
        retain_until: i64,
        reader: impl AsyncFnOnce(
            &str,
            &CompletionScope<'_>,
        ) -> Result<Option<CurrentAuthorization>, StoreError>,
    ) -> Result<CommittedFlacUpload<'inventory, 'receipts>, UploadError> {
        self.commit_flac(
            writer,
            auth,
            readback.0,
            retain_until,
            reader,
            async |until| sender.verify_readback(readback.0, readback.1, until).await,
        )
        .await
    }

    pub(super) async fn commit_flac<'inventory, 'receipts: 'inventory>(
        self,
        writer: &mut Client,
        auth: Authorization<'_>,
        inventory: &'inventory CompletedReadbackInventory<'receipts>,
        retain_until: i64,
        reader: impl AsyncFnOnce(
            &str,
            &CompletionScope<'_>,
        ) -> Result<Option<CurrentAuthorization>, StoreError>,
        verify: impl AsyncFnOnce(
            SystemTime,
        ) -> Result<
            VerifiedCompletedReadback<'inventory, 'receipts>,
            EnrolledReadbackError,
        >,
    ) -> Result<CommittedFlacUpload<'inventory, 'receipts>, UploadError> {
        self.prepared.authorize(auth)?;
        let target = self.target.target();
        let media = inventory.media();
        if (inventory.bucket(), media.object_key, media.configuration)
            != (
                target.bucket,
                target.key,
                self.prepared.intent.identities[4],
            )
        {
            return Err(Refused);
        }
        let verified = verify(self.target.usable_until())
            .await
            .map_err(|_| Unconfirmed)?;
        if !verified.has_flac_seek() {
            return Err(Refused);
        }
        let outcome = self
            .commit(writer, auth, &verified, retain_until, reader)
            .await?;
        if !verified.matches_committed_outcome(&outcome) {
            return Err(Unconfirmed);
        }
        Ok(CommittedFlacUpload { outcome, verified })
    }
}
