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
use nntune_ingest::{
    digest_candidate_store::{append_inventory, CandidateIntent, InventoryAppendError},
    multipart::{
        readback_inventory::{
            BucketReadbackResponse, CompletedReadbackInventory, InventoryError, ReadbackScope,
        },
        sidecar::{CompletedSidecar, SidecarPut, SidecarPutReply},
        CompletedVersionedMedia, Plan, UploadTarget, VersionedCompletionReply,
    },
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
const PAGE_ONE_KEY: &str = concat!(
    "media-digests/abababababab40ab80ababababababab/cdcdcdcdcdcd40cd80cdcdcdcdcdcdcd/",
    "06060606060640068006060606060606/00000001",
);
const PAGE_ONE_VERSION: &str = "second\\\"%2F+=";
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
    page_bytes: Vec<Vec<u8>>,
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
        let mut partition = plan.partition();
        let mut parts = Vec::new();
        let mut page_bytes = Vec::new();
        let chunks = plaintext.chunks(PLAIN_CHUNK_SIZE as usize);
        let chunk_count = chunks.len();
        for (index, chunk) in chunks.enumerate() {
            let sealed = enrollment.seal_chunk(chunk).unwrap();
            page_bytes.extend(sealed.page);
            parts.extend(
                partition
                    .push(&SealedChunk {
                        digest: Sha256::digest(&sealed.bytes).into(),
                        bytes: sealed.bytes,
                        final_chunk: index + 1 == chunk_count,
                    })
                    .unwrap(),
            );
        }
        parts.extend(partition.finish().unwrap());
        let mut media_bytes = Vec::new();
        let mut composite = Sha256::new();
        let uploads = parts
            .iter()
            .map(|part| {
                let (_, _, bytes, checksum) = part.request();
                media_bytes.extend_from_slice(bytes);
                composite.update(STANDARD.decode(checksum).unwrap());
                part.verify_reply("synthetic".into(), checksum).unwrap()
            })
            .collect();
        let composite = STANDARD.encode(composite.finalize());
        let body = format!(
            "<CompleteMultipartUploadResult><Bucket>{BUCKET}</Bucket><Key>{KEY}</Key>\
             <ChecksumSHA256>{composite}-{}</ChecksumSHA256></CompleteMultipartUploadResult>",
            parts.len()
        );
        let reply = VersionedCompletionReply {
            configuration: CONFIGURATION,
            status: 200,
            body: body.as_bytes(),
            version_headers: &[SHARED_VERSION],
        };
        let completion = plan.complete(uploads).unwrap();
        let media = completion.verify_versioned_reply(&context, reply).unwrap();
        Self {
            origin: enrollment.finish().unwrap(),
            context,
            record,
            media,
            media_bytes,
            page_bytes,
        }
    }

    fn sidecar(&self) -> CompletedSidecar<'_> {
        self.sidecar_at(0, PAGE_KEY, FINAL_VERSION)
    }

    fn sidecar_at(&self, index: u32, key: &str, version: &str) -> CompletedSidecar<'_> {
        let reply = SidecarPutReply {
            configuration: CONFIGURATION,
            bucket: BUCKET,
            key,
            status: 200,
            body: b"",
            version_headers: &[version],
        };
        let page = &self.page_bytes[index as usize];
        let put =
            SidecarPut::new(BUCKET, &self.context, &self.origin, MAP_ID, index, page).unwrap();
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
            PAGE_KEY => (PAGE_KEY, FINAL_VERSION, &self.page_bytes[0]),
            PAGE_ONE_KEY => (PAGE_ONE_KEY, PAGE_ONE_VERSION, &self.page_bytes[1]),
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
    use EnrolledReadbackError::Body;
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
            let invalid_flac = BodyReadbackError::Content(ReadbackError::MediaValidation);
            assert_eq!(result.err(), Some(Body(invalid_flac)));
        }
    }
}

#[tokio::test]
#[ignore = "requires a migrated disposable PostgreSQL database"]
async fn postgres_append_stages_only_exact_live_candidate_inventory() {
    use tokio_postgres::{Client, NoTls, Transaction};
    use InventoryAppendError::{Conflict, Identity, MissingOrFenced};
    async fn connect(url: &str) -> Client {
        let (client, connection) = tokio_postgres::connect(url, NoTls).await.unwrap();
        tokio::spawn(async move { connection.await.unwrap() });
        client
    }

    async fn insert_candidate(
        tx: &Transaction<'_>,
        ids: &[String; 5],
        map: &str,
        hashes: &[Vec<u8>],
    ) {
        tx.query_one(
            "SELECT public.store_storage_digest_candidate(
                $1::text::uuid, $2::text::uuid, $3::text::uuid, $4::text::uuid, 1, $5)",
            &[&ids[0], &ids[1], &ids[2], &map, &hashes],
        )
        .await
        .unwrap();
    }

    let url = std::env::var("NNTUNE_TEST_DATABASE_URL").expect("scratch database URL");
    assert!(
        std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
            && std::env::var("RUNNER_ENVIRONMENT").as_deref() == Ok("github-hosted")
            && url == "postgresql://postgres:ci-throwaway@localhost:5432/nntune_ci",
        "disposable GitHub-hosted migration database required"
    );
    let observer = connect(&url).await;
    let mut writer = connect(&url).await;

    // Put the audio frames after the first digest page boundary using FLAC padding.
    let mut plaintext = FLAC[..42].to_vec();
    plaintext[4] = 0;
    plaintext.extend_from_slice(&[0x81, 0xff, 0xff, 0xff]);
    plaintext.resize(46 + 0xff_ffff, 0);
    plaintext.extend_from_slice(&FLAC[42..]);
    let fixture = ReadbackFixture::new(&plaintext);
    let receipts = [
        fixture.sidecar_at(1, PAGE_ONE_KEY, PAGE_ONE_VERSION),
        fixture.sidecar(),
    ];
    let inventory = fixture.inventory(&receipts);
    let verified = inventory
        .verify(SCOPE, fixture.envelope(), async |_, pin| {
            Ok(fixture.response(pin))
        })
        .await
        .unwrap();
    let intent = CandidateIntent {
        identities: fixture.context.identities[1..].try_into().unwrap(),
        plaintext_len: fixture.context.plaintext_len,
        map_id: MAP_ID,
        header: fixture.context.header,
        reservation_generation: 1,
    };
    let ids = intent.identities.map(hex::encode);
    let map = hex::encode(MAP_ID);
    let length = intent.plaintext_len;
    let envelope = inventory.geometry().sealed_len();
    let header = hex::encode(intent.header);
    let digest = hex::encode(fixture.origin.plaintext_sha256());
    observer.batch_execute(&format!(
        "INSERT INTO public.storage_environments VALUES ('{0}');
         INSERT INTO public.storage_library_namespaces VALUES ('{1}', '{0}', NULL);
         INSERT INTO public.storage_object_intents VALUES ('{2}', '{3}', '{0}', '{1}', '{4}', {length}, {envelope}, decode('{header}', 'hex'));
         INSERT INTO public.storage_plaintext_digest_claim_intents VALUES ('{2}', '{0}', '{1}', {length}, decode('{digest}', 'hex'));
         SELECT public.reserve_storage_digest('{2}', 3600);",
        ids[0], ids[1], ids[2], ids[3], ids[4],
    )).await.unwrap();
    let mut hashes: Vec<_> = fixture
        .origin
        .candidate_page_hashes(&intent.identities, length, &MAP_ID, &intent.header)
        .unwrap()
        .iter()
        .map(|hash| hash.to_vec())
        .collect();

    let mut tx = writer.transaction().await.unwrap();
    for (bucket, configuration) in [("other-bucket", CONFIGURATION), (BUCKET, id(8))] {
        let scope = ReadbackScope {
            bucket,
            configuration,
        };
        assert_eq!(
            append_inventory(&mut tx, &intent, scope, &verified).await,
            Err(Identity)
        );
    }
    hashes.swap(0, 1);
    insert_candidate(&tx, &ids, &map, &hashes).await;
    assert_eq!(
        append_inventory(&mut tx, &intent, SCOPE, &verified).await,
        Err(MissingOrFenced)
    );
    tx.rollback().await.unwrap();

    hashes.swap(0, 1);
    let tx = writer.transaction().await.unwrap();
    insert_candidate(&tx, &ids, &map, &hashes).await;
    tx.commit().await.unwrap();

    let lookup = "SELECT media_version, page_versions FROM public.storage_digest_inventory_intents WHERE map_id = $1::text::uuid";
    let mut tx = writer.transaction().await.unwrap();
    // The insert trigger flips the fixture clock within this transaction.
    tx.batch_execute(
        "CREATE TEMP SEQUENCE inventory_insert_attempt;
         CREATE FUNCTION public.clock_timestamp() RETURNS timestamptz LANGUAGE sql VOLATILE AS $$
             SELECT CASE WHEN is_called THEN 'infinity' ELSE '-infinity' END::timestamptz
               FROM pg_temp.inventory_insert_attempt
         $$;
         CREATE FUNCTION public.test_mark_inventory_insert() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN PERFORM nextval('pg_temp.inventory_insert_attempt'); RETURN NEW; END $$;
         CREATE TRIGGER mark_inventory_insert BEFORE INSERT ON public.storage_digest_inventory_intents
         FOR EACH ROW EXECUTE FUNCTION public.test_mark_inventory_insert();
         SET LOCAL search_path = public, pg_catalog;",
    ).await.unwrap();
    assert_eq!(
        append_inventory(&mut tx, &intent, SCOPE, &verified).await,
        Err(MissingOrFenced)
    );
    assert!(tx
        .query_one(
            "SELECT is_called FROM pg_temp.inventory_insert_attempt",
            &[]
        )
        .await
        .unwrap()
        .get::<_, bool>(0));
    assert!(tx.query_opt(lookup, &[&map]).await.unwrap().is_none());
    tx.rollback().await.unwrap();

    let mut tx = writer.transaction().await.unwrap();
    append_inventory(&mut tx, &intent, SCOPE, &verified)
        .await
        .unwrap();
    assert!(tx.query_opt(lookup, &[&map]).await.unwrap().is_some());
    assert!(observer.query_opt(lookup, &[&map]).await.unwrap().is_none());
    tx.rollback().await.unwrap();

    let mut tx = writer.transaction().await.unwrap();
    append_inventory(&mut tx, &intent, SCOPE, &verified)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut tx = writer.transaction().await.unwrap();
    assert_eq!(
        append_inventory(&mut tx, &intent, SCOPE, &verified).await,
        Err(Conflict)
    );
    tx.rollback().await.unwrap();
    let row = observer.query_one(lookup, &[&map]).await.unwrap();
    assert_eq!(row.get::<_, String>(0), SHARED_VERSION);
    assert_eq!(
        row.get::<_, Vec<String>>(1),
        [FINAL_VERSION, PAGE_ONE_VERSION]
    );
}

#[tokio::test]
async fn completed_readback_rejects_foreign_scope_and_response_pins() {
    use BodyReadbackError::{Response, Transport};
    use EnrolledReadbackError::{Body, Envelope};
    let fixture = ReadbackFixture::new(FLAC);
    let foreign = ReadbackFixture::new(FLAC);
    let receipts = [fixture.sidecar()];
    let inventory = fixture.inventory(&receipts);
    let bad_scope = || Body(BodyReadbackError::Inventory);
    for (bucket, configuration, envelope, expected) in [
        (
            "other-bucket",
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

    for version in [Some(FINAL_VERSION), Some(SHARED_VERSION), None] {
        let result = inventory
            .verify(SCOPE, fixture.envelope(), async |_, pin| {
                let mut response = fixture.response(pin);
                if pin.object_key == PAGE_KEY {
                    let version = version.ok_or(io::ErrorKind::TimedOut)?;
                    if version == FINAL_VERSION {
                        response.bucket = "other-bucket".into();
                    }
                    response.response.version = Some(version.into());
                    response.response.body = MemoryBody(None);
                }
                Ok(response)
            })
            .await;
        let expected = version.map_or(Transport, |_| Response);
        assert_eq!(result.err(), Some(Body(expected)));
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
