use super::*;
use nntune_core::{digest_enrollment::EnrollmentOutput, media::Header};
use nntune_ingest::multipart::{
    session_completion::{CompletionRequest, DispatchError},
    CompletedVersionedMedia, MultipartError, UploadPart,
};
use std::fmt::Write as _;

const ETAGS: [&str; 2] = ["\"fixture-first\"", "\"fixture-tail\""];

/// Synthetic provider state for an upload whose two parts already have receipts.
struct ProviderInventory {
    parts: [Vec<u8>; 2],
    requested_markers: Vec<Option<u32>>,
    returned_parts: Vec<u32>,
    completed: bool,
}

impl ProviderInventory {
    fn part(&self, index: usize) -> CheckpointPart<'static> {
        CheckpointPart {
            number: index as u32 + 1,
            len: self.parts[index].len(),
            etag: ETAGS[index],
            digest: Sha256::digest(&self.parts[index]).into(),
        }
    }

    fn read_page(
        &mut self,
        marker: Option<u32>,
        fail_later_page: bool,
    ) -> io::Result<(CheckpointPart<'static>, Option<u32>)> {
        self.requested_markers.push(marker);
        let (part, next) = match marker {
            None => (self.part(0), Some(1)),
            Some(1) if fail_later_page => return Err(io::ErrorKind::ConnectionReset.into()),
            Some(1) => (self.part(1), None),
            _ => panic!("unexpected fixture continuation marker"),
        };
        self.returned_parts.push(part.number);
        Ok((part, next))
    }

    // Model the provider's inventory read inside the completion request.
    // The real completion caller must reject the provider's failed reply.
    fn complete(
        &mut self,
        request: CompletionRequest<'_>,
        configuration: [u8; 16],
        key: &str,
        fail_later_page: bool,
    ) -> CompletionResponse {
        assert!(!self.completed);
        assert_eq!(request.configuration, configuration);
        assert_eq!(
            (
                request.target.bucket,
                request.target.key,
                request.target.upload_id
            ),
            (BUCKET, key, UPLOAD),
        );
        let (first, marker) = self.read_page(None, fail_later_page).unwrap();
        assert_eq!(marker, Some(1));
        let Ok((second, None)) = self.read_page(marker, fail_later_page) else {
            return CompletionResponse {
                configuration,
                status: 503,
                version: None,
                body: b"<Error><Code>ServiceUnavailable</Code></Error>".to_vec(),
            };
        };

        let mut manifest = String::from(
            "<CompleteMultipartUpload xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">",
        );
        let mut composite = Sha256::new();
        for part in [first, second] {
            write!(
                manifest,
                "<Part><PartNumber>{}</PartNumber><ETag>&quot;{}&quot;</ETag>\
                 <ChecksumSHA256>{}</ChecksumSHA256></Part>",
                part.number,
                part.etag.trim_matches('"'),
                STANDARD.encode(part.digest),
            )
            .unwrap();
            composite.update(part.digest);
        }
        manifest.push_str("</CompleteMultipartUpload>");
        assert_eq!(request.body, manifest.as_bytes());

        self.completed = true;
        let composite = STANDARD.encode(composite.finalize());
        CompletionResponse {
            configuration,
            status: 200,
            version: Some(MEDIA.into()),
            body: format!(
                "<CompleteMultipartUploadResult><Bucket>{BUCKET}</Bucket><Key>{key}</Key>\
                 <ChecksumSHA256>{composite}-2</ChecksumSHA256></CompleteMultipartUploadResult>"
            )
            .into_bytes(),
        }
    }
}

fn partition<'a>(plan: Plan<'a>, sealed: &[EnrollmentOutput]) -> [UploadPart<'a>; 2] {
    let mut partition = plan.partition();
    let mut parts = Vec::new();
    for (index, chunk) in sealed.iter().enumerate() {
        parts.extend(
            partition
                .push(&SealedChunk {
                    digest: Sha256::digest(&chunk.bytes).into(),
                    bytes: chunk.bytes.clone(),
                    final_chunk: index + 1 == sealed.len(),
                })
                .unwrap(),
        );
    }
    parts.extend(partition.finish().unwrap());
    parts.try_into().ok().expect("expected two fixture parts")
}

async fn dispatch(
    opened: &OpenedUpload<'_>,
    (header, sealed): (Header, &[EnrollmentOutput]),
    saved: &[CheckpointPart<'_>; 2],
    provider: &mut ProviderInventory,
    key: &str,
    fail_later_page: bool,
) -> Result<CompletedVersionedMedia, DispatchError> {
    let prepared = opened.prepared;
    let context = prepared.context;
    let target = opened.durable_target().target();
    let plan = Plan::new(target, header, context.plaintext_len).unwrap();
    let [_, _, _, object, generation, configuration] = context.identities;
    let binding = ResumeBinding {
        plan,
        configuration,
        environment: prepared.job.environment,
        library: prepared.job.library_id,
        session: prepared.scope.session,
        object: &uuid_text(object),
        generation: &uuid_text(generation),
        envelope_scheme: 2,
    };
    let checkpoint = ResumeCheckpoint {
        binding,
        parts: saved,
    };
    // Rebuild the requests against the current durable target using the
    // retained ciphertext and the saved confirmations. No PUT is sent.
    let receipts = partition(plan, sealed)
        .iter()
        .zip(saved)
        .map(|(part, saved)| {
            part.verify_reply(saved.etag.into(), &STANDARD.encode(saved.digest))
                .unwrap()
        })
        .collect();
    SessionCompletion::prepare(binding, context, receipts, &checkpoint)
        .unwrap()
        .dispatch(async |request| {
            Ok(provider.complete(request, configuration, key, fail_later_page))
        })
        .await
}

async fn reservation(observer: &Client, opened: &OpenedUpload<'_>) -> String {
    let target = opened.durable_target().target();
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
        .get(0)
}

pub(super) async fn complete<'a>(
    observer: &Client,
    mut opened: OpenedUpload<'a>,
    key: &'a str,
    header: Header,
    sealed: &[EnrollmentOutput],
    fail_later_page: bool,
) -> (
    OpenedUpload<'a>,
    Option<Client>,
    CompletedVersionedMedia,
    Vec<u8>,
) {
    let context = opened.prepared.context;
    let original_target = opened.durable_target().target();
    let plan = Plan::new(original_target, header, context.plaintext_len).unwrap();
    let parts = partition(plan, sealed);
    let mut provider = ProviderInventory {
        parts: parts.each_ref().map(|part| part.request().2.to_vec()),
        requested_markers: Vec::new(),
        returned_parts: Vec::new(),
        completed: false,
    };
    let saved = [0, 1].map(|index| {
        let part = &parts[index];
        let reported = provider.part(index);
        let (_, number, bytes, _) = part.request();
        let receipt = part
            .verify_reply(reported.etag.into(), &STANDARD.encode(reported.digest))
            .unwrap();
        assert_eq!(receipt.etag(), ETAGS[index]);
        CheckpointPart {
            number,
            len: bytes.len(),
            etag: ETAGS[index],
            digest: Sha256::digest(bytes).into(),
        }
    });
    drop(parts);

    let mut resumed_writer = None;
    if fail_later_page {
        let before = reservation(observer, &opened).await;
        assert!(matches!(
            dispatch(&opened, (header, sealed), &saved, &mut provider, key, true).await,
            Err(DispatchError::Unconfirmed(Some(
                MultipartError::CompletionStatus
            )))
        ));
        assert_eq!(provider.requested_markers, [None, Some(1)]);
        assert_eq!(provider.returned_parts, [1]);
        assert!(!provider.completed);
        assert!([provider.part(0), provider.part(1)] == saved);
        assert_eq!(reservation(observer, &opened).await, before);

        let mut reconnect = connect(WRITER).await;
        let deadline = opened.durable_target().usable_until();
        let recovered = handoff(
            &mut reconnect,
            &opened.prepared.scope,
            &opened.lease,
            context,
            (BUCKET, key),
            deadline,
            async |_, _| panic!("resume initiated a replacement provider upload"),
        )
        .await
        .unwrap();
        assert!(recovered.target() == opened.durable_target().target());
        assert_eq!(recovered.usable_until(), deadline);
        opened.target = recovered;
        resumed_writer = Some(reconnect);
        assert_eq!(reservation(observer, &opened).await, before);
        provider.requested_markers.clear();
        provider.returned_parts.clear();
    }

    let completed = dispatch(&opened, (header, sealed), &saved, &mut provider, key, false)
        .await
        .unwrap();
    assert_eq!(provider.requested_markers, [None, Some(1)]);
    assert_eq!(provider.returned_parts, [1, 2]);
    assert!(provider.completed);
    (opened, resumed_writer, completed, provider.parts.concat())
}
