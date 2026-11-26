//! Assembles a one-part encrypted staging fixture with the signed S3 adapters.

use crate::{
    handlers::StoreError,
    ingest_session_caller::{
        self, Authorization, CommittedFlacUpload, CurrentAuthorization, PreparedUpload,
    },
    storage_digest_read::uuid_text,
};
use nntune_core::{
    digest_enrollment::{DigestEnrollment, OriginManifest},
    digest_readback::EnrolledEnvelope,
    media::{Header, PLAIN_CHUNK_SIZE},
    media_envelope::{verify_enrolled_scheme2, wrap_scheme2, EnvelopeContext},
    origin_sealer::SealedChunk,
};
use nntune_ingest::{
    digest_candidate_store::CandidateIntent,
    multipart::{
        readback_inventory::CompletedReadbackInventory,
        resume_checkpoint::{CheckpointPart, ResumeBinding, ResumeCheckpoint},
        session_completion::SessionCompletion,
        session_outcome::CompletionScope,
        sidecar::SidecarPut,
        Plan,
    },
};
use sha2::{Digest, Sha256};
use std::time::SystemTime;
use tokio_postgres::Client;

use super::TrialError::{self, Refused, Unconfirmed};

#[path = "sealed_fixture_storage.rs"]
mod storage;
pub(crate) use storage::TrialStorage;

pub struct SealedFixture {
    pub context: EnvelopeContext,
    pub intent: CandidateIntent,
    pub record: [u8; 230],
    header: Header,
    origin: OriginManifest,
    chunks: Vec<SealedChunk>,
    page: Vec<u8>,
}

impl SealedFixture {
    pub fn new(
        plaintext: &[u8],
        identities: [[u8; 16]; 5],
        map_id: [u8; 16],
        root_version: [u8; 16],
        root: &[u8; 32],
    ) -> Result<Self, TrialError> {
        if plaintext.len() != super::FIXTURE_BYTES
            || <[u8; 32]>::from(Sha256::digest(plaintext)) != super::caller::FIXTURE_DIGEST
        {
            return Err(Refused);
        }
        let plaintext_len = plaintext.len() as u64;
        let mut enrollment =
            DigestEnrollment::new(plaintext_len, identities, map_id).map_err(|_| Unconfirmed)?;
        let header = enrollment.header();
        let [environment, library, object, generation, configuration] = identities;
        let context = EnvelopeContext {
            identities: [
                root_version,
                environment,
                library,
                object,
                generation,
                configuration,
            ],
            plaintext_len,
            header: header.to_bytes(),
        };
        let record = wrap_scheme2(root, &context, enrollment.key().expose_secret())
            .map_err(|_| Unconfirmed)?;
        let input = plaintext.chunks(PLAIN_CHUNK_SIZE as usize);
        let count = input.len();
        let mut chunks = Vec::with_capacity(count);
        let mut page = None;
        for (index, input) in input.enumerate() {
            let sealed = enrollment.seal_chunk(input).map_err(|_| Unconfirmed)?;
            let final_chunk = index + 1 == count;
            if final_chunk != sealed.page.is_some() {
                return Err(Unconfirmed);
            }
            page = sealed.page.or(page);
            chunks.push(SealedChunk {
                digest: Sha256::digest(&sealed.bytes).into(),
                bytes: sealed.bytes,
                final_chunk,
            });
        }
        let origin = enrollment.finish().map_err(|_| Unconfirmed)?;
        let intent = CandidateIntent {
            identities,
            plaintext_len,
            map_id,
            header: context.header,
            reservation_generation: 1,
        };
        Ok(Self {
            context,
            intent,
            record,
            header,
            origin,
            chunks,
            page: page.ok_or(Unconfirmed)?,
        })
    }

    pub fn origin(&self) -> &OriginManifest {
        &self.origin
    }

    /// Run only after the exact candidate and reservation are durable, and
    /// after the owner, redeemed invite and current upload grant pass preflight.
    pub(crate) async fn send<T>(
        &self,
        writer: &mut Client,
        auth: Authorization<'_>,
        prepared: PreparedUpload<'_>,
        sender: &impl TrialStorage,
        bucket: &str,
        root: &[u8; 32],
        until: SystemTime,
        retain_until: i64,
        reload: impl AsyncFnOnce(
            &str,
            &CompletionScope<'_>,
        ) -> Result<Option<CurrentAuthorization>, StoreError>,
        consume: impl AsyncFnOnce(&mut Client, &CommittedFlacUpload<'_, '_>) -> Result<T, TrialError>,
    ) -> Result<T, TrialError> {
        let [environment, library, object, generation, configuration] = self.intent.identities;
        if prepared.context.identities != self.context.identities
            || prepared.context.header != self.context.header
            || prepared.context.plaintext_len != self.context.plaintext_len
            || prepared.intent.map_id != self.intent.map_id
            || prepared.intent.reservation_generation != self.intent.reservation_generation
            || !sender.matches_storage(configuration, bucket)
        {
            return Err(Refused);
        }
        self.verify_root(root)?;
        let [environment, library, object, generation] =
            [environment, library, object, generation].map(uuid_text);
        let key = format!("media/{object}/{generation}");
        let opened = ingest_session_caller::open(
            writer,
            auth,
            prepared,
            until,
            async |db, scope, lease, deadline| {
                sender
                    .open_upload(db, scope, lease, prepared.context, &key, deadline)
                    .await
            },
        )
        .await
        .map_err(upload_error)?;
        let upload = opened.durable_target();
        let until = upload.usable_until();
        let target = upload.target();
        if target.bucket != bucket {
            return Err(Unconfirmed);
        }
        let plan =
            Plan::new(target, self.header, self.context.plaintext_len).map_err(|_| Unconfirmed)?;
        let mut partition = plan.partition();
        for chunk in &self.chunks {
            if partition.push(chunk).map_err(|_| Unconfirmed)?.is_some() {
                return Err(Unconfirmed);
            }
        }
        let part = partition
            .finish()
            .map_err(|_| Unconfirmed)?
            .ok_or(Unconfirmed)?;
        let binding = ResumeBinding {
            plan,
            configuration,
            environment: &environment,
            library: &library,
            session: prepared.scope.session,
            object: &object,
            generation: &generation,
            envelope_scheme: 2,
        };
        let uploaded = sender
            .put_part(binding, &self.context, &part, upload, until)
            .await
            .map_err(|_| Unconfirmed)?;
        let (_, number, bytes, _) = part.request();
        let etag = uploaded.etag().to_owned();
        let parts = [CheckpointPart {
            number,
            len: bytes.len(),
            etag: &etag,
            digest: Sha256::digest(bytes).into(),
        }];
        let checkpoint = ResumeCheckpoint {
            binding,
            parts: &parts,
        };
        let completion =
            SessionCompletion::prepare(binding, &self.context, vec![uploaded], &checkpoint)
                .map_err(|_| Unconfirmed)?;
        let completed = sender
            .complete(upload, completion)
            .await
            .map_err(|_| Unconfirmed)?;
        sender
            .confirm_head(&completed, &self.context, until)
            .await
            .map_err(|_| Unconfirmed)?;
        let map = self.intent.map_id;
        let put = SidecarPut::new(bucket, &self.context, &self.origin, map, 0, &self.page)
            .map_err(|_| Unconfirmed)?;
        let sidecars = [sender
            .put_sidecar(put, until)
            .await
            .map_err(|_| Unconfirmed)?];
        let inventory = CompletedReadbackInventory::assemble(
            bucket,
            configuration,
            &self.context,
            map,
            &self.origin,
            &completed,
            &sidecars,
        )
        .map_err(|_| Unconfirmed)?;
        let envelope = EnrolledEnvelope {
            authorized_root: root,
            context: &self.context,
            trusted_record: &self.record,
            retained_record: &self.record,
        };
        let committed = opened
            .commit_flac(
                writer,
                auth,
                &inventory,
                retain_until,
                reload,
                async |deadline| sender.verify_readback(&inventory, envelope, deadline).await,
            )
            .await
            .map_err(upload_error)?;
        consume(writer, &committed).await
    }

    fn verify_root(&self, root: &[u8; 32]) -> Result<(), TrialError> {
        verify_enrolled_scheme2(
            root,
            &self.context,
            &self.record,
            &self.record,
            &self.origin,
        )
        .map_err(|_| Refused)
    }
}

fn upload_error(error: ingest_session_caller::UploadError) -> TrialError {
    match error {
        ingest_session_caller::UploadError::Refused => Refused,
        ingest_session_caller::UploadError::Unconfirmed => Unconfirmed,
    }
}

#[cfg(test)]
pub(crate) fn verify_sealed_fixture(input: &[u8]) {
    let id = |byte: u8| {
        let mut value = [byte; 16];
        value[6] = 0x40;
        value[8] = 0x80;
        value
    };
    let map = id(6);
    let root = id(7);
    let ids = [1, 2, 3, 4, 5].map(id);
    let fixture = SealedFixture::new(input, ids, map, root, &[0xab; 32]).unwrap();
    assert_eq!(fixture.verify_root(&[0xab; 32]), Ok(()));
    assert_eq!(fixture.verify_root(&[0xac; 32]), Err(Refused));
    assert_eq!(fixture.context.identities, [7, 1, 2, 3, 4, 5].map(id));
    assert_eq!(fixture.header.to_bytes(), fixture.context.header);
    assert_eq!(
        fixture.origin().plaintext_sha256(),
        &super::caller::FIXTURE_DIGEST
    );
    let hashes = fixture
        .origin()
        .candidate_page_hashes(&ids, input.len() as u64, &map, &fixture.context.header)
        .unwrap();
    assert_eq!(
        hashes,
        &[nntune_core::digest_page::page_hash(&fixture.page).unwrap()]
    );
    assert_eq!(fixture.chunks.len(), 3);
    assert!(fixture
        .chunks
        .iter()
        .take(2)
        .all(|chunk| !chunk.final_chunk));
    assert!(fixture.chunks[2].final_chunk);
    assert_ne!(&fixture.chunks[0].bytes[..64], &input[..64]);
    let mut changed = input.to_vec();
    changed[0] ^= 1;
    assert!(matches!(
        SealedFixture::new(&changed, ids, map, root, &[0xab; 32]),
        Err(Refused)
    ));
}
