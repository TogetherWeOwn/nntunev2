use base64::{engine::general_purpose::STANDARD, Engine};
use nntune_core::{
    media::{ciphertext_size, HEADER_SIZE, TAG_SIZE},
    media_envelope::EnvelopeContext,
    origin_sealer::{OriginSealer, MAX_NEW_PLAINTEXT},
    s3_locator::{S3Binding, S3Locator},
};
use nntune_ingest::multipart::{
    head_receipt::{compare_head_receipt, HeadReceipt, HeadReceiptError::*},
    MultipartError, Plan, UploadTarget, VersionedCompletionReply,
};
use sha2::{Digest, Sha256};

const BUCKET: &str = "test-bucket";
const KEY: &str = "media/abababababab40ab80ababababababab/cdcdcdcdcdcd40cd80cdcdcdcdcdcdcd";

fn id(byte: u8) -> [u8; 16] {
    let mut value = [byte; 16];
    value[6] = 0x40;
    value[8] = 0x80;
    value
}

#[test]
fn pins_only_the_exact_versioned_completion_response() {
    use MultipartError::*;

    let mut sealer = OriginSealer::new(1).unwrap();
    let target = UploadTarget {
        bucket: BUCKET,
        key: KEY,
        upload_id: "synthetic",
    };
    let plan = Plan::new(target, sealer.header(), 1).unwrap();
    let mut partition = plan.partition();
    assert!(partition
        .push(&sealer.seal_chunk(&[42]).unwrap())
        .unwrap()
        .is_none());
    let part = partition.finish().unwrap().unwrap();
    let (_, _, bytes, checksum) = part.request();
    let digest = STANDARD.encode(Sha256::digest(Sha256::digest(bytes)));
    let completion = plan
        .complete(vec![part
            .verify_reply("opaque-part".into(), checksum)
            .unwrap()])
        .unwrap();
    let bucket = format!("<Bucket>{BUCKET}</Bucket>");
    let key = format!("<Key>{KEY}</Key>");
    let response = |fields: &str| {
        format!("<CompleteMultipartUploadResult>{fields}<ChecksumSHA256>{digest}-1</ChecksumSHA256></CompleteMultipartUploadResult>")
    };
    let body = response(&format!("{bucket}{key}"));
    let context = EnvelopeContext {
        identities: [0, 1, 2, 0xab, 0xcd, 5].map(id),
        plaintext_len: 1,
        header: sealer.header().to_bytes(),
    };
    let check =
        |context: &EnvelopeContext, configuration, status, body: &str, version_headers: &[&str]| {
            completion.verify_versioned_reply(
                context,
                VersionedCompletionReply {
                    configuration,
                    status,
                    body: body.as_bytes(),
                    version_headers,
                },
            )
        };
    let version = "Opaque+%2F/=";
    let pinned = check(&context, id(5), 200, &body, &[version]).unwrap();
    assert_eq!(pinned.metadata().locator().bucket(), BUCKET);
    assert_eq!(pinned.metadata().locator().key(), KEY);
    assert!(matches!(
        pinned.metadata().locator().binding(),
        S3Binding::VersionId(actual) if actual == version
    ));
    assert!(pinned.metadata().matches_context(&context));
    assert_eq!(
        pinned.metadata().ciphertext_len(),
        HEADER_SIZE + 1 + TAG_SIZE
    );

    let long = "v".repeat(1025);
    for headers in [
        &[][..],
        &[version, version],
        &[""],
        &["null"],
        &["v1, v2"],
        &[" v1"],
        &["é"],
        &[long.as_str()],
    ] {
        assert_eq!(
            check(&context, id(5), 200, &body, headers).err(),
            Some(CompletionVersion)
        );
    }
    for (reply_body, error) in [
        (response(&bucket), CompletionIdentity),
        (response(&key), CompletionIdentity),
        (body.replace(BUCKET, "other-bucket"), CompletionBody),
        (body.replace("-1</", "-2</"), Checksum),
        ("<Error/>".into(), EmbeddedError),
    ] {
        assert_eq!(
            check(&context, id(5), 200, &reply_body, &[version]).err(),
            Some(error)
        );
    }
    assert_eq!(
        check(&context, id(5), 503, &body, &[version]).err(),
        Some(CompletionStatus)
    );
    assert_eq!(
        check(&context, id(6), 200, &body, &[version]).err(),
        Some(CompletionIdentity)
    );
    let mut wrong_generation = EnvelopeContext { ..context };
    wrong_generation.identities[4] = id(0xef);
    let mut wrong_header = EnvelopeContext { ..context };
    wrong_header.header[47] ^= 1;
    for wrong_context in [
        wrong_generation,
        wrong_header,
        EnvelopeContext {
            plaintext_len: 2,
            ..context
        },
    ] {
        assert_eq!(
            check(&wrong_context, id(5), 200, &body, &[version]).err(),
            Some(CompletionIdentity)
        );
    }
}

#[test]
fn compares_only_bounded_completion_and_head_metadata() {
    let mut sealer = OriginSealer::new(1).unwrap();
    let target = UploadTarget {
        bucket: BUCKET,
        key: KEY,
        upload_id: "synthetic",
    };
    let plan = Plan::new(target, sealer.header(), 1).unwrap();
    for invalid in [0, MAX_NEW_PLAINTEXT + 1, u64::MAX] {
        assert!(matches!(
            Plan::new(target, sealer.header(), invalid),
            Err(MultipartError::Geometry)
        ));
    }
    let mut partition = plan.partition();
    assert!(partition
        .push(&sealer.seal_chunk(&[42]).unwrap())
        .unwrap()
        .is_none());
    let part = partition.finish().unwrap().unwrap();
    let (_, _, bytes, checksum) = part.request();
    let digest = Sha256::digest(Sha256::digest(bytes));
    let part = part.verify_reply("opaque-part".into(), checksum).unwrap();
    let completion = plan.complete(vec![part]).unwrap();
    let receipt_xml = |fields: &str| {
        format!("<CompleteMultipartUploadResult>{fields}<ChecksumSHA256>{}-1</ChecksumSHA256></CompleteMultipartUploadResult>", STANDARD.encode(digest))
    };
    let body = receipt_xml("");
    let context = EnvelopeContext {
        identities: [0, 1, 2, 0xab, 0xcd, 5].map(id),
        plaintext_len: 1,
        header: sealer.header().to_bytes(),
    };
    let binding = S3Binding::Etag("\"opaque-object\"".into());
    let locate = |bucket: &str, key: &str| {
        S3Locator::new(bucket.into(), key.into(), binding.clone()).unwrap()
    };
    let expected = locate(BUCKET, KEY);
    let observed = locate(BUCKET, KEY);
    let length = HEADER_SIZE + 1 + TAG_SIZE;
    let head = || HeadReceipt {
        configuration: Some(id(5)),
        locator: Some(&observed),
        content_length: Some(length),
    };
    let compare = |xml: &str, want: &S3Locator, receipt: Option<HeadReceipt<'_>>| {
        compare_head_receipt(&completion, 200, xml.as_bytes(), &context, want, receipt)
    };
    let check = |receipt: Option<HeadReceipt<'_>>| compare(&body, &expected, receipt);
    assert_eq!(check(Some(head())), Ok(()));
    let bucket_field = format!("<Bucket>{BUCKET}</Bucket>");
    let key_field = format!("<Key>{KEY}</Key>");
    for fields in [
        &bucket_field,
        &key_field,
        &format!("{bucket_field}{key_field}"),
    ] {
        assert_eq!(
            compare(&receipt_xml(fields), &expected, Some(head())),
            Ok(())
        );
    }
    let other_key = KEY.replace("cd", "ef");
    for fields in [
        format!("<Bucket>other-bucket</Bucket>{key_field}"),
        format!("{bucket_field}<Key>{other_key}</Key>"),
        bucket_field.repeat(2),
        key_field.repeat(2),
    ] {
        assert_eq!(
            compare(&receipt_xml(&fields), &expected, Some(head())),
            Err(Completion(MultipartError::CompletionBody))
        );
    }
    assert_eq!(check(None), Err(MissingHead));
    assert_eq!(
        compare("<Error/>", &expected, Some(head())),
        Err(Completion(MultipartError::EmbeddedError))
    );
    assert_eq!(
        compare(&body, &locate("other-bucket", KEY), Some(head())),
        Err(ExpectedObject)
    );
    let other_header = OriginSealer::new(1).unwrap().header().to_bytes();
    assert_ne!(other_header, context.header);
    for wrong_context in [
        EnvelopeContext {
            header: other_header,
            ..context
        },
        EnvelopeContext {
            plaintext_len: 2,
            ..context
        },
    ] {
        assert_eq!(
            compare_head_receipt(
                &completion,
                200,
                body.as_bytes(),
                &wrong_context,
                &expected,
                Some(head())
            ),
            Err(ExpectedObject)
        );
    }
    let mut other_context = EnvelopeContext { ..context };
    other_context.identities[4] = id(0xef);
    let other_locator = locate(BUCKET, &other_key);
    assert_eq!(
        compare_head_receipt(
            &completion,
            200,
            body.as_bytes(),
            &other_context,
            &other_locator,
            Some(HeadReceipt {
                locator: Some(&other_locator),
                ..head()
            })
        ),
        Err(ExpectedObject)
    );

    for missing in [
        HeadReceipt {
            configuration: None,
            ..head()
        },
        HeadReceipt {
            locator: None,
            ..head()
        },
        HeadReceipt {
            content_length: None,
            ..head()
        },
    ] {
        assert_eq!(check(Some(missing)), Err(MissingMetadata));
    }
    for wrong in [
        locate("other-bucket", KEY),
        locate(BUCKET, &KEY.replace("abababababab", "dededededede")),
        locate(BUCKET, &KEY.replace("cdcdcdcdcdcd", "efefefefefef")),
        S3Locator::new(
            BUCKET.into(),
            KEY.into(),
            S3Binding::Etag("\"different\"".into()),
        )
        .unwrap(),
        S3Locator::new(
            BUCKET.into(),
            KEY.into(),
            S3Binding::VersionId("other-version".into()),
        )
        .unwrap(),
    ] {
        let receipt = HeadReceipt {
            locator: Some(&wrong),
            ..head()
        };
        assert_eq!(check(Some(receipt)), Err(WrongObject));
    }
    assert_eq!(
        check(Some(HeadReceipt {
            configuration: Some(id(6)),
            ..head()
        })),
        Err(WrongObject)
    );
    for (size, error) in [
        (0, InvalidSize),
        (length - 1, InvalidSize),
        (length + 1, SizeMismatch),
        (ciphertext_size(MAX_NEW_PLAINTEXT).unwrap() + 1, InvalidSize),
        (u64::MAX, InvalidSize),
    ] {
        assert_eq!(
            check(Some(HeadReceipt {
                content_length: Some(size),
                ..head()
            })),
            Err(error)
        );
    }
}
