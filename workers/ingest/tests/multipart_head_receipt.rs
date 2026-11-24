use base64::{engine::general_purpose::STANDARD, Engine};
use nntune_core::{
    media::{ciphertext_size, HEADER_SIZE, TAG_SIZE},
    media_envelope::EnvelopeContext,
    origin_sealer::{OriginSealer, MAX_NEW_PLAINTEXT},
    s3_locator::{S3Binding, S3Locator},
};
use nntune_ingest::multipart::{
    head_receipt::{compare_head_receipt, HeadReceipt, HeadReceiptError::*},
    resume_checkpoint::{CheckpointPart, ResumeBinding, ResumeCheckpoint},
    session_completion::{
        CompletionRequest, CompletionResponse, DispatchError, PrepareError, SessionCompletion,
    },
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

#[tokio::test]
async fn session_completion_requires_checkpoint_full_manifest_and_head() {
    let mut sealer = OriginSealer::new(1).unwrap();
    let target = UploadTarget {
        bucket: BUCKET,
        key: KEY,
        upload_id: "synthetic+/=opaque",
    };
    let plan = Plan::new(target, sealer.header(), 1).unwrap();
    let chunk = sealer.seal_chunk(&[42]).unwrap();
    let make_part = |plan: Plan<'static>| {
        let mut partition = plan.partition();
        assert!(partition.push(&chunk).unwrap().is_none());
        partition.finish().unwrap().unwrap()
    };
    let part = make_part(plan);
    let (_, number, bytes, checksum) = part.request();
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    let saved = CheckpointPart {
        number,
        len: bytes.len(),
        etag: "\"opaque&part<1>\"",
        digest,
    };
    let context = EnvelopeContext {
        identities: [0, 1, 2, 0xab, 0xcd, 5].map(id),
        plaintext_len: 1,
        header: sealer.header().to_bytes(),
    };
    let [environment, library, object, generation] =
        [1, 2, 0xab, 0xcd].map(|byte| hex::encode(id(byte)));
    let binding = ResumeBinding {
        plan,
        configuration: id(5),
        environment: &environment,
        library: &library,
        session: "03030303-0303-4003-8003-030303030303",
        object: &object,
        generation: &generation,
        envelope_scheme: 2,
    };
    let parts = [saved];
    let checkpoint = ResumeCheckpoint {
        binding,
        parts: &parts,
    };
    let receipts = || vec![part.verify_reply(saved.etag.into(), checksum).unwrap()];
    assert!(matches!(
        SessionCompletion::prepare(
            binding,
            &context,
            receipts(),
            &ResumeCheckpoint {
                binding: ResumeBinding {
                    session: "09090909-0909-4009-8009-090909090909",
                    ..binding
                },
                ..checkpoint
            }
        ),
        Err(PrepareError::CheckpointMismatch)
    ));
    let mut stale_part = saved;
    stale_part.digest[0] ^= 1;
    assert!(matches!(
        SessionCompletion::prepare(
            binding,
            &context,
            receipts(),
            &ResumeCheckpoint {
                parts: &[stale_part],
                ..checkpoint
            }
        ),
        Err(PrepareError::CheckpointMismatch)
    ));
    assert!(matches!(
        SessionCompletion::prepare(
            binding,
            &context,
            vec![],
            &ResumeCheckpoint {
                parts: &[],
                ..checkpoint
            }
        ),
        Err(PrepareError::Parts(MultipartError::Geometry))
    ));
    let wrong_scopes = [1, 2, 3, 4, 5].map(|index| {
        let mut wrong_scope = EnvelopeContext { ..context };
        wrong_scope.identities[index] = id(9);
        wrong_scope
    });
    let mut wrong_header = EnvelopeContext { ..context };
    wrong_header.header[47] ^= 1;
    let wrong_length = EnvelopeContext {
        plaintext_len: 2,
        ..context
    };
    let mut invalid_configuration = EnvelopeContext { ..context };
    invalid_configuration.identities[5][6] = 0;
    let invalid_metadata = [wrong_header, wrong_length, invalid_configuration];
    for wrong_context in wrong_scopes.into_iter().chain(invalid_metadata) {
        assert!(matches!(
            SessionCompletion::prepare(binding, &wrong_context, receipts(), &checkpoint),
            Err(PrepareError::EnvelopeContextMismatch)
        ));
    }
    for wrong_target in [
        UploadTarget {
            key: "media/efefefefefef40ef80efefefefefefef/cdcdcdcdcdcd40cd80cdcdcdcdcdcdcd",
            ..target
        },
        UploadTarget {
            bucket: "invalid_bucket",
            ..target
        },
    ] {
        let wrong_plan = Plan::new(wrong_target, sealer.header(), 1).unwrap();
        let wrong_part = make_part(wrong_plan);
        let wrong_binding = ResumeBinding {
            plan: wrong_plan,
            ..binding
        };
        assert!(matches!(
            SessionCompletion::prepare(
                wrong_binding,
                &context,
                vec![wrong_part
                    .verify_reply(saved.etag.into(), checksum)
                    .unwrap()],
                &ResumeCheckpoint {
                    binding: wrong_binding,
                    ..checkpoint
                }
            ),
            Err(PrepareError::EnvelopeContextMismatch)
        ));
    }
    let prepare =
        || SessionCompletion::prepare(binding, &context, receipts(), &checkpoint).unwrap();
    let completion = prepare();
    assert!(completion.manifest().eq([(1, saved.etag, checksum)]));
    let body = format!(
        "<CompleteMultipartUploadResult><Bucket>{BUCKET}</Bucket><Key>{KEY}</Key><ChecksumSHA256>{}-1</ChecksumSHA256></CompleteMultipartUploadResult>",
        STANDARD.encode(Sha256::digest(digest))
    );
    let locate = |version: &str| {
        S3Locator::new(
            BUCKET.into(),
            KEY.into(),
            S3Binding::VersionId(version.into()),
        )
        .unwrap()
    };
    let observed = locate("pinned-version");
    let wrong_version = locate("other-version");
    let head = |locator| HeadReceipt {
        configuration: Some(id(5)),
        locator: Some(locator),
        content_length: Some(HEADER_SIZE + 1 + TAG_SIZE),
    };
    let verify = |body: &[u8], version_headers: &[&str]| {
        completion.verify_completion(VersionedCompletionReply {
            configuration: id(5),
            status: 200,
            body,
            version_headers,
        })
    };
    let completed = verify(body.as_bytes(), &["pinned-version"]).unwrap();
    assert!(completed.metadata().locator() == &observed);
    assert_eq!(
        completed.verify_head(&context, Some(head(&observed))),
        Ok(())
    );
    assert_eq!(completed.verify_head(&context, None), Err(MissingHead));
    assert_eq!(
        completed.verify_head(&context, Some(head(&wrong_version))),
        Err(WrongObject)
    );
    let mut wrong_context = EnvelopeContext { ..context };
    wrong_context.identities[5] = id(6);
    assert_eq!(
        completed.verify_head(&wrong_context, Some(head(&observed))),
        Err(ExpectedObject)
    );
    assert_eq!(
        verify(body.as_bytes(), &[]).err(),
        Some(MultipartError::CompletionVersion)
    );
    assert_eq!(
        verify(b"<Error/>", &["pinned-version"]).err(),
        Some(MultipartError::EmbeddedError)
    );

    let expect_request = |request: CompletionRequest<'_>| {
        assert_eq!(request.configuration, id(5));
        assert!(request.target == target);
        let expected = format!(
            "<CompleteMultipartUpload xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
             <Part><PartNumber>1</PartNumber><ETag>&quot;opaque&amp;part&lt;1&gt;&quot;</ETag>\
             <ChecksumSHA256>{checksum}</ChecksumSHA256></Part></CompleteMultipartUpload>"
        );
        assert_eq!(request.body.as_slice(), expected.as_bytes());
    };
    let reply = |body: &[u8], version: &str| CompletionResponse {
        configuration: id(5),
        status: 200,
        body: body.into(),
        version: Some(version.into()),
    };
    let sent = prepare()
        .dispatch(async |request| {
            expect_request(request);
            let declared = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{body}");
            Ok(reply(declared.as_bytes(), "pinned-version"))
        })
        .await
        .unwrap();
    assert!(sent.metadata().locator() == &observed);

    for (response, expected) in [
        (Err(()), None),
        (
            Ok(reply(b"<Error/>", "pinned-version")),
            Some(MultipartError::EmbeddedError),
        ),
        (
            Ok(reply(body.as_bytes(), "pinned-version, other")),
            Some(MultipartError::CompletionVersion),
        ),
    ] {
        let mut sends = 0;
        let result = prepare()
            .dispatch(async |request| {
                expect_request(request);
                sends += 1;
                response
            })
            .await;
        assert_eq!(result.err(), Some(DispatchError::Unconfirmed(expected)));
        assert_eq!(sends, 1);
    }

    for (upload_id, etag) in [
        ("invalid\nvalue", saved.etag),
        (target.upload_id, "invalid\0value"),
    ] {
        let plan = Plan::new(
            UploadTarget {
                upload_id,
                ..target
            },
            sealer.header(),
            1,
        )
        .unwrap();
        let binding = ResumeBinding { plan, ..binding };
        let checkpoint = ResumeCheckpoint {
            binding,
            parts: &[CheckpointPart { etag, ..saved }],
        };
        let part = make_part(plan).verify_reply(etag.into(), checksum).unwrap();
        let result = SessionCompletion::prepare(binding, &context, vec![part], &checkpoint)
            .unwrap()
            .dispatch(async |_| panic!("invalid request was dispatched"))
            .await;
        assert_eq!(result.err(), Some(DispatchError::InvalidRequest));
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
