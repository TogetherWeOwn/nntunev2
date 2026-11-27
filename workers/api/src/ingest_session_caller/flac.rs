//! Off-route FLAC handoff and completion evidence for the encrypted upload producer.

use std::ops::Range;

use nntune_core::{media_envelope::EnvelopeContext, s3_locator::S3MediaMetadata};
use nntune_ingest::multipart::{
    readback_inventory::VerifiedCompletedReadback, session_outcome::CommittedOutcome,
};

#[cfg(any(test, target_arch = "wasm32"))]
use std::time::{Duration, SystemTime, UNIX_EPOCH};
#[cfg(any(target_arch = "wasm32", all(test, feature = "postgres-tests")))]
use {
    super::{
        Authorization, CompletionScope, CurrentAuthorization, OpenedUpload, Refused, StoreError,
        Unconfirmed, UploadError,
    },
    nntune_core::digest_readback::EnrolledReadbackError,
    nntune_ingest::multipart::readback_inventory::CompletedReadbackInventory,
    tokio_postgres::Client,
};
#[cfg(target_arch = "wasm32")]
use {
    nntune_core::digest_readback::EnrolledEnvelope,
    nntune_ingest::multipart::s3_completion::S3CompletionSender,
};

pub(super) fn matches_outcome(
    context: &EnvelopeContext,
    map: [u8; 16],
    outcome: &CommittedOutcome,
) -> bool {
    let [root, _, _, object, generation, configuration] = context.identities;
    [
        (&outcome.map, map),
        (&outcome.object, object),
        (&outcome.generation, generation),
        (&outcome.configuration, configuration),
        (&outcome.root_version, root),
    ]
    .into_iter()
    .all(|(actual, expected)| {
        let mut expected = format!("{:032x}", u128::from_be_bytes(expected));
        for boundary in [20, 16, 12, 8] {
            expected.insert(boundary, '-');
        }
        actual == &expected
    })
}

#[cfg(any(test, target_arch = "wasm32"))]
pub(super) fn readback_deadline(
    now: SystemTime,
    expires: u64,
    target_deadline: SystemTime,
) -> Option<SystemTime> {
    let deadline = target_deadline.min(UNIX_EPOCH.checked_add(Duration::from_secs(expires))?);
    (now < deadline).then_some(deadline)
}

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

impl<'inventory, 'receipts> CommittedFlacUpload<'inventory, 'receipts> {
    pub(crate) fn verified(&self) -> &VerifiedCompletedReadback<'inventory, 'receipts> {
        &self.verified
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

    pub(crate) async fn commit_flac<'inventory, 'receipts: 'inventory>(
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
        let expires = self.prepared.authorize(auth)?;
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
        let now = UNIX_EPOCH + Duration::from_millis(crate::worker_entry::now_millis());
        let until =
            readback_deadline(now, expires as u64, self.target.usable_until()).ok_or(Refused)?;
        let verified = verify(until).await.map_err(|_| Unconfirmed)?;
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
