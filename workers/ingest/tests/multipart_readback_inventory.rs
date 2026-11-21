use base64::{engine::general_purpose::STANDARD, Engine};
use nntune_core::{
    digest_enrollment::{DigestEnrollment, OriginManifest},
    digest_readback::{
        BodyReadbackError, EnrolledEnvelope, EnrolledReadbackError, ReadbackBody, ReadbackError,
        ReadbackPin, ReadbackResponse,
    },
    media::PLAIN_CHUNK_SIZE,
    media_envelope::{wrap_scheme2, EnvelopeContext},
    origin_sealer::SealedChunk,
};
use nntune_ingest::multipart::{
    readback_inventory::{
        BucketReadbackResponse, CompletedReadbackInventory, InventoryError, ReadbackScope,
    },
    sidecar::{CompletedSidecar, SidecarPut, SidecarPutReply},
    CompletedVersionedMedia, Plan, UploadTarget, VersionedCompletionReply,
};
use sha2::{Digest, Sha256};
use std::io;

const BUCKET: &str = "test-bucket";
const KEY: &str = "media/abababababab40ab80ababababababab/cdcdcdcdcdcd40cd80cdcdcdcdcdcdcd";
const SHARED_VERSION: &str = "Opaque+%2F/=";
const FINAL_VERSION: &str = "Final%2f+==";

const fn id(byte: u8) -> [u8; 16] {
    let mut value = [byte; 16];
    value[6] = 0x40;
    value[8] = 0x80;
    value
}

const PAGE_KEY: &str = concat!(
    "media-digests/abababababab40ab80ababababababab/cdcdcdcdcdcd40cd80cdcdcdcdcdcdcd/",
    "06060606060640068006060606060606/00000000",
);
const ROOT: [u8; 32] = [17; 32];
const CONFIGURATION: [u8; 16] = id(5);
const MAP_ID: [u8; 16] = id(6);
const SCOPE: ReadbackScope<'_> = ReadbackScope {
    bucket: BUCKET,
    configuration: CONFIGURATION,
};
// Mono, 8-bit, 19 constant samples at 44.1 kHz.
const FLAC: &[u8] = b"fLaC\x80\0\0\x22\0\x10\0\x10\0\0\0\0\0\0\x0a\xc4\x40\x70\0\0\0\x13\
    \x46\xd2\xbe\x27\x5e\xd2\xdd\x86\xab\x2b\x53\x88\xf7\x34\x6d\x15\
    \xff\xf8\x69\x02\0\x0f\xb7\0\x2a\x7a\xdd\xff\xf8\x69\x02\x01\x02\x81\0\x2a\x9d\x1d";

struct MemoryBody<'a>(Option<&'a [u8]>);

impl ReadbackBody for MemoryBody<'_> {
    async fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
        io::Read::read(self.0.as_mut().expect("rejected response was read"), dst)
    }
}

struct ReadbackFixture {
    origin: OriginManifest,
    context: EnvelopeContext,
    record: [u8; 230],
    media: CompletedVersionedMedia,
    media_bytes: Vec<u8>,
    page_bytes: Vec<u8>,
}

impl ReadbackFixture {
    fn new(plaintext: &[u8]) -> Self {
        let identities = [0, 1, 2, 0xab, 0xcd, 5].map(id);
        let plaintext_len = plaintext.len() as u64;
        let mut enrollment =
            DigestEnrollment::new(plaintext_len, identities[1..].try_into().unwrap(), MAP_ID)
                .unwrap();
        let context = EnvelopeContext {
            identities,
            plaintext_len,
            header: enrollment.header().to_bytes(),
        };
        let record = wrap_scheme2(&ROOT, &context, enrollment.key().expose_secret()).unwrap();
        let plan = Plan::new(
            UploadTarget {
                bucket: BUCKET,
                key: KEY,
                upload_id: "synthetic",
            },
            enrollment.header(),
            plaintext_len,
        )
        .unwrap();
        let sealed = enrollment.seal_chunk(plaintext).unwrap();
        let mut partition = plan.partition();
        assert!(partition
            .push(&SealedChunk {
                digest: Sha256::digest(&sealed.bytes).into(),
                bytes: sealed.bytes,
                final_chunk: true,
            })
            .unwrap()
            .is_none());
        let part = partition.finish().unwrap().unwrap();
        let (_, _, bytes, checksum) = part.request();
        let composite = STANDARD.encode(Sha256::digest(STANDARD.decode(checksum).unwrap()));
        let body = format!(
            "<CompleteMultipartUploadResult><Bucket>{BUCKET}</Bucket><Key>{KEY}</Key>\
             <ChecksumSHA256>{composite}-1</ChecksumSHA256></CompleteMultipartUploadResult>"
        );
        let reply = VersionedCompletionReply {
            configuration: CONFIGURATION,
            status: 200,
            body: body.as_bytes(),
            version_headers: &[SHARED_VERSION],
        };
        let uploaded = part.verify_reply("synthetic".into(), checksum).unwrap();
        let completion = plan.complete(vec![uploaded]).unwrap();
        let media = completion.verify_versioned_reply(&context, reply).unwrap();
        Self {
            origin: enrollment.finish().unwrap(),
            context,
            record,
            media,
            media_bytes: bytes.to_vec(),
            page_bytes: sealed.page.unwrap(),
        }
    }

    fn sidecar(&self) -> CompletedSidecar<'_> {
        let reply = SidecarPutReply {
            configuration: CONFIGURATION,
            bucket: BUCKET,
            key: PAGE_KEY,
            status: 200,
            body: b"",
            version_headers: &[FINAL_VERSION],
        };
        let page = &self.page_bytes;
        let put = SidecarPut::new(BUCKET, &self.context, &self.origin, MAP_ID, 0, page).unwrap();
        put.verify_reply(reply).unwrap()
    }

    fn inventory<'a>(
        &'a self,
        sidecars: &'a [CompletedSidecar<'_>],
    ) -> CompletedReadbackInventory<'a> {
        CompletedReadbackInventory::assemble(
            BUCKET,
            CONFIGURATION,
            &self.context,
            MAP_ID,
            &self.origin,
            &self.media,
            sidecars,
        )
        .unwrap()
    }

    fn envelope(&self) -> EnrolledEnvelope<'_> {
        EnrolledEnvelope {
            authorized_root: &ROOT,
            context: &self.context,
            trusted_record: &self.record,
            retained_record: &self.record,
        }
    }

    fn response(&self, pin: ReadbackPin<'_>) -> BucketReadbackResponse<MemoryBody<'_>> {
        let (key, version, bytes) = match pin.object_key {
            KEY => (KEY, SHARED_VERSION, &self.media_bytes),
            PAGE_KEY => (PAGE_KEY, FINAL_VERSION, &self.page_bytes),
            _ => panic!("unexpected readback key"),
        };
        assert_eq!(pin.configuration, CONFIGURATION);
        assert_eq!(pin.version, Some(version));
        BucketReadbackResponse {
            bucket: BUCKET.into(),
            response: ReadbackResponse {
                configuration: CONFIGURATION,
                object_key: key.into(),
                version: Some(version.into()),
                status: 200,
                content_length: Some(bytes.len() as u64),
                has_content_range: false,
                has_content_encoding: false,
                body: MemoryBody(Some(bytes)),
            },
        }
    }
}

#[tokio::test]
async fn completed_readback_binds_the_exact_inventory_after_flac_and_eof() {
    for (plaintext, complete) in [(FLAC, true), (&FLAC[..FLAC.len() - 1], false)] {
        let fixture = ReadbackFixture::new(plaintext);
        let receipts = [fixture.sidecar()];
        let inventory = fixture.inventory(&receipts);
        let result = inventory
            .verify(SCOPE, fixture.envelope(), async |bucket, pin| {
                assert_eq!(bucket, BUCKET);
                Ok(fixture.response(pin))
            })
            .await;
        if complete {
            let verified = result.unwrap();
            assert!(std::ptr::eq(verified.inventory(), &inventory));
            assert!(std::ptr::eq(verified.origin(), &fixture.origin));
        } else {
            assert_eq!(
                result.err(),
                Some(EnrolledReadbackError::Body(BodyReadbackError::Content(
                    ReadbackError::MediaValidation
                ),))
            );
        }
    }
}

#[tokio::test]
async fn completed_readback_rejects_foreign_scope_and_response_pins() {
    use EnrolledReadbackError::{Body, Envelope};

    let fixture = ReadbackFixture::new(FLAC);
    let foreign = ReadbackFixture::new(FLAC);
    let receipts = [fixture.sidecar()];
    let inventory = fixture.inventory(&receipts);
    let foreign_bucket = "other-bucket";
    let bad_scope = || Body(BodyReadbackError::Inventory);
    for (bucket, configuration, envelope, expected) in [
        (
            foreign_bucket,
            CONFIGURATION,
            fixture.envelope(),
            bad_scope(),
        ),
        (BUCKET, id(8), fixture.envelope(), bad_scope()),
        (BUCKET, CONFIGURATION, foreign.envelope(), Envelope),
    ] {
        let scope = ReadbackScope {
            bucket,
            configuration,
        };
        let result = inventory
            .verify::<MemoryBody<'_>>(scope, envelope, async |_, _| {
                panic!("scope or origin mismatch opened an object")
            })
            .await;
        assert_eq!(result.err(), Some(expected));
    }

    for (bucket, version) in [(foreign_bucket, FINAL_VERSION), (BUCKET, SHARED_VERSION)] {
        let result = inventory
            .verify(SCOPE, fixture.envelope(), async |_, pin| {
                let mut response = fixture.response(pin);
                if pin.object_key == PAGE_KEY {
                    response.bucket = bucket.into();
                    response.response.version = Some(version.into());
                    response.response.body = MemoryBody(None);
                }
                Ok(response)
            })
            .await;
        assert_eq!(result.err(), Some(Body(BodyReadbackError::Response)));
    }
}

#[test]
fn assembles_only_complete_scoped_receipts_in_page_order() {
    let plaintext_len = 256 * PLAIN_CHUNK_SIZE + 1;
    let identities = [0, 1, 2, 0xab, 0xcd, 5].map(id);
    let map_id = id(6);
    let mut enrollment =
        DigestEnrollment::new(plaintext_len, identities[1..].try_into().unwrap(), map_id).unwrap();
    let context = EnvelopeContext {
        identities,
        plaintext_len,
        header: enrollment.header().to_bytes(),
    };
    let plan = Plan::new(
        UploadTarget {
            bucket: BUCKET,
            key: KEY,
            upload_id: "synthetic",
        },
        enrollment.header(),
        plaintext_len,
    )
    .unwrap();
    let mut partition = plan.partition();
    let mut uploads = Vec::new();
    let mut pages = Vec::new();
    let chunk = vec![42; PLAIN_CHUNK_SIZE as usize];
    for index in 0..=256 {
        let final_chunk = index == 256;
        let input = if final_chunk { &chunk[..1] } else { &chunk };
        let sealed = enrollment.seal_chunk(input).unwrap();
        if let Some(page) = sealed.page {
            pages.push(page);
        }
        let sealed = SealedChunk {
            digest: Sha256::digest(&sealed.bytes).into(),
            bytes: sealed.bytes,
            final_chunk,
        };
        uploads.extend(partition.push(&sealed).unwrap());
    }
    uploads.extend(partition.finish().unwrap());
    let manifest = enrollment.finish().unwrap();
    let mut composite = Sha256::new();
    let parts = uploads
        .iter()
        .map(|upload| {
            let (_, _, _, checksum) = upload.request();
            composite.update(STANDARD.decode(checksum).unwrap());
            upload.verify_reply("opaque-part".into(), checksum).unwrap()
        })
        .collect();
    let checksum = STANDARD.encode(composite.finalize());
    let body = format!(
        "<CompleteMultipartUploadResult><Bucket>{BUCKET}</Bucket><Key>{KEY}</Key>\
         <ChecksumSHA256>{checksum}-{}</ChecksumSHA256></CompleteMultipartUploadResult>",
        uploads.len()
    );
    let completion = plan.complete(parts).unwrap();
    let complete_media = |context: &EnvelopeContext| {
        completion
            .verify_versioned_reply(
                context,
                VersionedCompletionReply {
                    configuration: context.identities[5],
                    status: 200,
                    body: body.as_bytes(),
                    version_headers: &[SHARED_VERSION],
                },
            )
            .unwrap()
    };
    let sidecar = |bucket: &'static str, index, version| {
        let put = SidecarPut::new(
            bucket,
            &context,
            &manifest,
            map_id,
            index,
            &pages[index as usize],
        )
        .unwrap();
        let key = put.request().0.key.clone();
        put.verify_reply(SidecarPutReply {
            configuration: identities[5],
            bucket,
            key: &key,
            status: 200,
            body: b"",
            version_headers: &[version],
        })
        .unwrap()
    };
    let media = complete_media(&context);
    let receipts = [
        sidecar(BUCKET, 1, FINAL_VERSION),
        sidecar(BUCKET, 0, SHARED_VERSION),
    ];
    let inventory = CompletedReadbackInventory::assemble(
        BUCKET,
        identities[5],
        &context,
        map_id,
        &manifest,
        &media,
        &receipts,
    )
    .unwrap();
    assert_eq!(inventory.bucket(), BUCKET);
    assert_eq!(
        inventory.geometry().sealed_len(),
        plaintext_len + 48 + 257 * 16
    );
    assert_eq!(inventory.media().object_key, KEY);
    assert_eq!(inventory.media().version, Some(SHARED_VERSION));
    let prefix = concat!(
        "media-digests/abababababab40ab80ababababababab/cdcdcdcdcdcd40cd80cdcdcdcdcdcdcd/",
        "06060606060640068006060606060606"
    );
    assert_eq!(inventory.pages().len(), 2);
    for (&(index, pin), (expected_index, length, suffix, version)) in
        inventory.pages().iter().zip([
            (0, 8_284, "00000000", SHARED_VERSION),
            (1, 124, "00000001", FINAL_VERSION),
        ])
    {
        assert_eq!(index, expected_index);
        assert_eq!(
            inventory.geometry().page(index).unwrap().encoded_len,
            length
        );
        assert_eq!(pin.configuration, identities[5]);
        assert_eq!(pin.object_key, format!("{prefix}/{suffix}"));
        assert_eq!(pin.version, Some(version));
    }

    let page0 = || sidecar(BUCKET, 0, SHARED_VERSION);
    let page1 = || sidecar(BUCKET, 1, FINAL_VERSION);
    for bad_receipts in [
        vec![page0()],
        vec![page0(), page0()],
        vec![page0(), page1(), page1()],
        vec![page0(), sidecar("other-bucket", 1, FINAL_VERSION)],
    ] {
        assert_eq!(
            CompletedReadbackInventory::assemble(
                BUCKET,
                identities[5],
                &context,
                map_id,
                &manifest,
                &media,
                &bad_receipts,
            )
            .err(),
            Some(InventoryError::Sidecars)
        );
    }

    let mut foreign_context = EnvelopeContext { ..context };
    foreign_context.identities[2] = id(8); // Same key, different library.
    let foreign_media = complete_media(&foreign_context);
    for (bucket, provider, map, media, error) in [
        (
            BUCKET,
            identities[5],
            map_id,
            &foreign_media,
            InventoryError::Media,
        ),
        (
            "other-bucket",
            identities[5],
            map_id,
            &media,
            InventoryError::Media,
        ),
        (BUCKET, id(8), map_id, &media, InventoryError::Origin),
        (BUCKET, identities[5], id(8), &media, InventoryError::Origin),
    ] {
        assert_eq!(
            CompletedReadbackInventory::assemble(
                bucket, provider, &context, map, &manifest, media, &receipts,
            )
            .err(),
            Some(error)
        );
    }
}
