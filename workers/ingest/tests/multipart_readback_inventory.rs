use base64::{engine::general_purpose::STANDARD, Engine};
use nntune_core::{
    digest_enrollment::DigestEnrollment, media::PLAIN_CHUNK_SIZE, media_envelope::EnvelopeContext,
    origin_sealer::SealedChunk,
};
use nntune_ingest::multipart::{
    readback_inventory::{CompletedReadbackInventory, InventoryError},
    sidecar::{SidecarPut, SidecarPutReply},
    Plan, UploadTarget, VersionedCompletionReply,
};
use sha2::{Digest, Sha256};

const BUCKET: &str = "test-bucket";
const KEY: &str = "media/abababababab40ab80ababababababab/cdcdcdcdcdcd40cd80cdcdcdcdcdcdcd";
const SHARED_VERSION: &str = "Opaque+%2F/=";
const FINAL_VERSION: &str = "Final%2f+==";

fn id(byte: u8) -> [u8; 16] {
    let mut value = [byte; 16];
    value[6] = 0x40;
    value[8] = 0x80;
    value
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
