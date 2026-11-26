use super::*;
use nntune_ingest::multipart::{
    resume_checkpoint::matches_checkpoint, CompletedVersionedMedia, UploadedPart,
};

/// The synthetic provider lists one confirmed part at a time, as with
/// ListParts and max-parts=1. The caller keeps its own verified receipts.
struct ProviderInventory<'a> {
    stored: [CheckpointPart<'a>; 2],
    requested_markers: Vec<Option<u32>>,
    returned_parts: Vec<u32>,
    fail_second_page: bool,
}

impl<'a> ProviderInventory<'a> {
    fn read_page(&mut self, marker: Option<u32>) -> io::Result<(CheckpointPart<'a>, Option<u32>)> {
        self.requested_markers.push(marker);
        let (part, next) = match marker {
            None => (self.stored[0], Some(self.stored[0].number)),
            Some(1) if self.fail_second_page => {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "fixture inventory read interrupted",
                ));
            }
            Some(1) => (self.stored[1], None),
            _ => panic!("unexpected fixture continuation marker"),
        };
        self.returned_parts.push(part.number);
        Ok((part, next))
    }
}

async fn complete_after_read<'a>(
    provider: &mut ProviderInventory<'_>,
    checkpoint: &ResumeCheckpoint<'a>,
    context: &'a EnvelopeContext,
    receipts: &mut Vec<UploadedPart<'a>>,
    send: impl AsyncFnOnce(CompletionRequest<'a>) -> Result<CompletionResponse, ()>,
) -> Result<CompletedVersionedMedia, UploadError> {
    let (first, next) = provider
        .read_page(None)
        .map_err(|_| UploadError::Unconfirmed)?;
    assert_eq!(next, Some(1));
    let (second, next) = provider
        .read_page(next)
        .map_err(|_| UploadError::Unconfirmed)?;
    assert_eq!(next, None);
    assert!([first, second].as_slice() == checkpoint.parts);

    let completion = SessionCompletion::prepare(
        checkpoint.binding,
        context,
        std::mem::take(receipts),
        checkpoint,
    )
    .unwrap();
    completion
        .dispatch(send)
        .await
        .map_err(|_| UploadError::Unconfirmed)
}

pub(super) async fn complete<'a>(
    observer: &Client,
    opened: &OpenedUpload<'a>,
    checkpoint: &ResumeCheckpoint<'a>,
    mut receipts: Vec<UploadedPart<'a>>,
    fail_later_page: bool,
    send: impl AsyncFnOnce(CompletionRequest<'a>) -> Result<CompletionResponse, ()>,
) -> CompletedVersionedMedia {
    let context = opened.prepared.context;
    let target = opened.durable_target().target();
    let deadline = opened.durable_target().usable_until();
    let stored = checkpoint.parts.try_into().unwrap();
    let mut provider = ProviderInventory {
        stored,
        requested_markers: Vec::new(),
        returned_parts: Vec::new(),
        fail_second_page: fail_later_page,
    };
    let saved_parts = checkpoint.parts.to_vec();
    assert!(matches_checkpoint(
        checkpoint.binding,
        &receipts,
        checkpoint
    ));

    if fail_later_page {
        let reservation = async || {
            observer
                .query_one(
                    "SELECT xmin::text FROM public.ingest_session_reservations
                 WHERE id = $1::text::uuid AND multipart_upload_id = $2
                   AND multipart_bucket = $3 AND write_lease_id = $4::text::uuid
                   AND terminal_reason IS NULL AND completed_map_id IS NULL
                   AND checkpoint_revision = 0 AND checkpoint_offset = 0
                   AND clock_timestamp() < LEAST(idle_deadline, absolute_deadline)
                   AND NOT EXISTS (SELECT FROM public.storage_digest_inventory_intents
                                   WHERE map_id = $5::text::uuid)",
                    &[
                        &opened.prepared.scope.session,
                        &target.upload_id,
                        &target.bucket,
                        &opened.lease,
                        &uuid_text(opened.prepared.intent.map_id),
                    ],
                )
                .await
                .expect("the exact upload must be active without a completion receipt")
                .get::<_, String>(0)
        };
        let before = reservation().await;
        assert!(matches!(
            complete_after_read(
                &mut provider,
                checkpoint,
                context,
                &mut receipts,
                async |_| { panic!("incomplete inventory reached completion dispatch") }
            )
            .await,
            Err(UploadError::Unconfirmed)
        ));
        assert_eq!(provider.requested_markers, [None, Some(1)]);
        assert_eq!(provider.returned_parts, [1]);
        assert!(checkpoint.parts == saved_parts.as_slice());
        assert!(matches_checkpoint(
            checkpoint.binding,
            &receipts,
            checkpoint
        ));
        assert_eq!(reservation().await, before);

        let mut reconnect = connect(WRITER).await;
        let resumed = handoff(
            &mut reconnect,
            &opened.prepared.scope,
            &opened.lease,
            context,
            (target.bucket, target.key),
            deadline,
            async |_| panic!("resume initiated a replacement provider upload"),
        )
        .await
        .unwrap();
        assert!(resumed.target() == target);
        assert_eq!(resumed.usable_until(), deadline);
        assert_eq!(reservation().await, before);
        provider.fail_second_page = false;
        provider.requested_markers.clear();
        provider.returned_parts.clear();
    }

    let completed = complete_after_read(&mut provider, checkpoint, context, &mut receipts, send)
        .await
        .unwrap();
    assert_eq!(provider.requested_markers, [None, Some(1)]);
    assert_eq!(provider.returned_parts, [1, 2]);
    completed
}
