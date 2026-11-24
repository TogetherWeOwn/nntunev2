use super::*;
use crate::native_rest::{issue_credential, Scope};
use base64::{engine::general_purpose::STANDARD, Engine};
use nntune_core::{
    digest_enrollment::DigestEnrollment,
    digest_readback::{EnrolledEnvelope, ReadbackBody, ReadbackResponse},
    media_envelope::wrap_scheme2,
    origin_sealer::SealedChunk,
};
use nntune_ingest::multipart::{
    initiation::InitiatedUpload,
    readback_inventory::{BucketReadbackResponse, CompletedReadbackInventory},
    resume_checkpoint::{CheckpointPart, ResumeBinding, ResumeCheckpoint},
    session_completion::{CompletionResponse, SessionCompletion},
    session_upload::handoff,
    sidecar::{SidecarPut, SidecarPutReply},
    Plan,
};
use sha2::{Digest, Sha256};
use std::{io, net::Ipv4Addr};
use tokio_postgres::{Config, NoTls};

const DB: &str = "postgresql://postgres:ci-throwaway@127.0.0.1:5432/nntune_upload_caller_ci";
const WRITER: &str =
    "postgresql://upload_fixture_writer:ci-throwaway@127.0.0.1:5432/nntune_upload_caller_ci";
const OWNER: &str = "synthetic-upload-caller";
const BUCKET: &str = "fixture-bucket";
const UPLOAD: &str = "fixture+/=?";
const MEDIA: &str = "media-fixture-version";
const PAGE: &str = "page-fixture-version";
// The same two-frame generated FLAC used by the signed S3 WASM fixture.
const FLAC: &[u8] = b"fLaC\x80\0\0\x22\0\x10\0\x10\0\0\0\0\0\0\x0a\xc4\x40\x70\0\0\0\x13\
    \x46\xd2\xbe\x27\x5e\xd2\xdd\x86\xab\x2b\x53\x88\xf7\x34\x6d\x15\
    \xff\xf8\x69\x02\0\x0f\xb7\0\x2a\x7a\xdd\xff\xf8\x69\x02\x01\x02\x81\0\x2a\x9d\x1d";

struct Body<'a>(&'a [u8]);

impl ReadbackBody for Body<'_> {
    async fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        io::Read::read(&mut self.0, buffer)
    }
}

async fn connect(url: &str) -> Client {
    assert_eq!(std::env::var("GITHUB_ACTIONS").as_deref(), Ok("true"));
    assert_eq!(
        std::env::var("RUNNER_ENVIRONMENT").as_deref(),
        Ok("github-hosted")
    );
    assert_eq!(std::env::var("NNTUNE_TEST_DATABASE_URL").as_deref(), Ok(DB));
    let mut config: Config = url.parse().unwrap();
    config.hostaddr(Ipv4Addr::LOCALHOST.into());
    config.connect_timeout(Duration::from_secs(5));
    let (client, connection) = config.connect(NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    client
}

#[tokio::test]
#[ignore = "requires the disposable hosted upload caller database"]
async fn scoped_writer_commits_and_replays_exact_upload() {
    let observer = connect(DB).await;
    observer
        .batch_execute(include_str!("../../src/ingest_session_schema.sql"))
        .await
        .unwrap();
    observer
        .batch_execute(include_str!("ingest_session_writer_grants.sql"))
        .await
        .unwrap();
    observer
        .batch_execute(include_str!("ingest_session_caller_grants.sql"))
        .await
        .unwrap();
    let mut writer = connect(WRITER).await;
    assert!(writer.query_one(
        "SELECT session_user = current_user AND current_user = 'upload_fixture_writer'
            AND NOT EXISTS (SELECT FROM pg_auth_members WHERE member = current_user::regrole)
            AND NOT (SELECT rolinherit OR rolsuper FROM pg_roles WHERE rolname = current_user)
            AND NOT has_table_privilege(current_user, 'public.ingest_session_reservations', 'DELETE,TRUNCATE')
            AND NOT has_any_column_privilege(current_user, 'public.track_sources', 'INSERT,UPDATE')
            AND NOT has_any_column_privilege(current_user, 'public.storage_validated_digest_references', 'INSERT,UPDATE')
            AND NOT has_table_privilege(current_user, 'public.track_sources', 'DELETE,TRUNCATE')",
        &[],
    ).await.unwrap().get::<_, bool>(0));

    for (index, fault) in [
        None,
        Some(Fault::Digest),
        Some(Fault::Revoked),
        Some(Fault::Retention),
    ]
    .into_iter()
    .enumerate()
    {
        prove_attempt(&observer, &mut writer, index as u8 * 16, fault).await;
    }
}

#[derive(Clone, Copy)]
enum Fault {
    Digest,
    Revoked,
    Retention,
}

async fn prove_attempt(observer: &Client, mut writer: &mut Client, seed: u8, fault: Option<Fault>) {
    let [root_id, environment, library, object, seal, configuration, map] = [1, 2, 3, 4, 5, 6, 7]
        .map(|value| {
            let mut id = [value + seed; 16];
            id[6] = 0x40;
            id[8] = 0x80;
            id
        });
    let identities = [environment, library, object, seal, configuration];
    let [env, lib, obj, generation, provider] = identities.map(uuid_text);
    let map_text = uuid_text(map);
    let root_text = uuid_text(root_id);
    let key = format!("media/{obj}/{generation}");
    let length = FLAC.len() as u64;
    let mut sealer = DigestEnrollment::new(length, identities, map).unwrap();
    let header = sealer.header();
    let context = EnvelopeContext {
        identities: [root_id, environment, library, object, seal, configuration],
        plaintext_len: length,
        header: header.to_bytes(),
    };
    let root = [17; 32];
    let record = wrap_scheme2(&root, &context, sealer.key().expose_secret()).unwrap();
    let sealed = sealer.seal_chunk(FLAC).unwrap();
    let page = sealed.page.unwrap();
    let origin = sealer.finish().unwrap();
    let intent = CandidateIntent {
        identities,
        plaintext_len: length,
        map_id: map,
        header: context.header,
        reservation_generation: 1,
    };
    let hashes = origin
        .candidate_page_hashes(&identities, length, &map, &context.header)
        .unwrap()
        .iter()
        .map(|hash| hash.as_slice())
        .collect::<Vec<_>>();
    observer.batch_execute(&format!(
        "INSERT INTO public.users (id, email, created_at) VALUES ('{OWNER}', '{OWNER}@example.invalid', 1) ON CONFLICT (id) DO NOTHING;
         INSERT INTO public.storage_environments VALUES ('{env}');
         INSERT INTO public.storage_library_namespaces VALUES ('{lib}', '{env}', '{OWNER}');
         INSERT INTO public.storage_object_intents VALUES ('{obj}', '{generation}', '{env}',
             '{lib}', '{provider}', {length}, {}, decode('{}', 'hex'));
         INSERT INTO public.storage_plaintext_digest_claim_intents VALUES ('{obj}', '{env}', '{lib}',
             {length}, decode('{}','hex'));
         SELECT public.reserve_storage_digest('{obj}', 3600);",
        nntune_core::media::ciphertext_size(length).unwrap(), hex(&context.header), hex(origin.plaintext_sha256())
    )).await.unwrap();
    observer.query_one(
        "SELECT public.store_storage_digest_candidate($1::text::uuid,$2::text::uuid,$3::text::uuid,$4::text::uuid,1,$5)",
        &[&env, &lib, &obj, &map_text, &hashes],
    ).await.unwrap();
    observer.execute(
        "INSERT INTO public.storage_object_envelope_intents VALUES ($1::text::uuid,$2::text::uuid,$3)",
        &[&obj, &root_text, &record.as_slice()],
    ).await.unwrap();

    let pepper = Pepper {
        version: 1,
        bytes: b"synthetic pepper",
    };
    let now = now_millis() / 1000;
    let credential = issue_credential(OWNER, &[Scope::IngestSubmit], 300, now, &pepper).unwrap();
    let grant = Grant {
        credential_id: &credential.record.id,
        environment: &env,
        library_id: &lib,
        actions: &[Action::Create, Action::Upload],
    };
    let auth = Authorization {
        record: &credential.record,
        bearer: &credential.display_once,
        peppers: &[pepper],
        grant: &grant,
    };
    let request = Request {
        file_bytes: length,
        content_sha256: match fault {
            Some(Fault::Digest) => [0xaa; 32],
            _ => *origin.plaintext_sha256(),
        },
        title: "synthetic",
        artist: "fixture",
        album: None,
    };
    let sid = fresh_id().unwrap();
    let fingerprint = request.fingerprint().unwrap();
    let reserve = include_str!("../../src/ingest_session_reserve.sql");
    writer
        .batch_execute(&format!(
        "CREATE OR REPLACE FUNCTION pg_temp.reserve(uuid, uuid, text, text, bytea, uuid) RETURNS SETOF uuid
         LANGUAGE sql AS $sql${reserve}$sql$;"
    ))
        .await
        .unwrap();
    let inserted: String = writer.query_one(
        "SELECT pg_temp.reserve($1::text::uuid, $2::text::uuid, $3, $4, $5, $6::text::uuid)::text",
        &[&env, &lib, &OWNER, &"fixture-key", &fingerprint.bytes().as_slice(), &sid],
    ).await.unwrap().get(0);
    assert_eq!(inserted.replace('-', ""), sid);
    let job = Job {
        id: &sid,
        owner_id: OWNER,
        environment: &env,
        library_id: &lib,
    };
    let prepared = PreparedUpload {
        scope: CompletionScope {
            environment,
            library,
            principal: OWNER,
            session: &sid,
            fingerprint: fingerprint.bytes(),
            seal,
            revision: 0,
            offset: 0,
        },
        job: &job,
        request: &request,
        context: &context,
        intent: &intent,
    };
    let until = SystemTime::now() + Duration::from_secs(45);

    let mut stranger = credential.record.clone();
    stranger.user_id = "foreign-principal".into();
    let foreign = Authorization {
        record: &stranger,
        ..auth
    };
    let mut revoked = credential.record.clone();
    revoked.revoked_at = Some(now);
    let wrong_environment = Grant {
        environment: "foreign",
        ..grant
    };
    let wrong_library = Grant {
        library_id: "foreign",
        ..grant
    };
    for wrong in [
        foreign,
        Authorization {
            record: &revoked,
            ..auth
        },
        Authorization {
            grant: &wrong_environment,
            ..auth
        },
        Authorization {
            grant: &wrong_library,
            ..auth
        },
    ] {
        assert!(matches!(
            open(
                &mut writer,
                wrong,
                prepared,
                until,
                async |_, _, _, _| panic!("refusal reached the provider")
            )
            .await,
            Err(UploadError::Refused)
        ));
        assert!(matches!(
            read(&mut writer, wrong, prepared).await,
            Err(UploadError::Refused)
        ));
    }
    // Use self-consistent grant and job for the other principal so that the
    // actual SQL ownership fence, rather than only the grant, refuses access.
    let other_job = Job {
        owner_id: "foreign-principal",
        ..job
    };
    let other = PreparedUpload {
        job: &other_job,
        scope: CompletionScope {
            principal: other_job.owner_id,
            ..prepared.scope
        },
        ..prepared
    };
    assert!(matches!(
        open(
            &mut writer,
            foreign,
            other,
            until,
            async |_, _, _, _| panic!("foreign principal reached the provider")
        )
        .await,
        Err(UploadError::Refused)
    ));

    let opened =
        open(
            &mut writer,
            auth,
            prepared,
            until,
            async |db, scope, lease, deadline| {
                handoff(
                    db,
                    scope,
                    lease,
                    &context,
                    (BUCKET, &key),
                    deadline,
                    async |_| {
                        let claimed = observer.query_one(
                "SELECT multipart_object_id IS NOT NULL AND multipart_upload_id IS NULL
                 FROM public.ingest_session_reservations WHERE id = $1::text::uuid", &[&sid],
            ).await.unwrap();
                        assert!(claimed.get::<_, bool>(0));
                        Ok(InitiatedUpload::verify(
                            configuration,
                            BUCKET,
                            &key,
                            200,
                            format!(
                "<InitiateMultipartUploadResult><Bucket>{BUCKET}</Bucket><Key>{key}</Key>
                    <UploadId>{UPLOAD}</UploadId></InitiateMultipartUploadResult>"
            )
                            .as_bytes(),
                        )
                        .unwrap())
                    },
                )
                .await
            },
        )
        .await
        .unwrap();
    let target = opened.durable_target().target();
    assert_eq!(target.upload_id, UPLOAD);
    let row = observer.query_one(
        "SELECT multipart_upload_id FROM public.ingest_session_reservations WHERE id = $1::text::uuid",
        &[&sid],
    ).await.unwrap();
    assert_eq!(row.get::<_, &str>(0), UPLOAD);
    assert!(matches!(
        open(
            &mut writer,
            auth,
            prepared,
            until,
            async |_, _, _, _| panic!("open replay reached the provider")
        )
        .await,
        Err(UploadError::Refused)
    ));

    let plan = Plan::new(target, header, length).unwrap();
    let mut partition = plan.partition();
    assert!(partition
        .push(&SealedChunk {
            digest: Sha256::digest(&sealed.bytes).into(),
            bytes: sealed.bytes,
            final_chunk: true
        })
        .unwrap()
        .is_none());
    let part = partition.finish().unwrap().unwrap();
    let (_, number, ciphertext, checksum) = part.request();
    assert_ne!(ciphertext, FLAC);
    let ciphertext = ciphertext.to_vec();
    let part_digest: [u8; 32] = Sha256::digest(&ciphertext).into();
    let receipt = part.verify_reply("\"synthetic\"".into(), checksum).unwrap();
    let binding = ResumeBinding {
        plan,
        configuration,
        environment: &env,
        library: &lib,
        session: &sid,
        object: &obj,
        generation: &generation,
        envelope_scheme: 2,
    };
    let parts = [CheckpointPart {
        number,
        len: ciphertext.len(),
        etag: "\"synthetic\"",
        digest: part_digest,
    }];
    let checkpoint = ResumeCheckpoint {
        binding,
        parts: &parts,
    };
    let completed = SessionCompletion::prepare(binding, &context, vec![receipt], &checkpoint)
        .unwrap()
        .dispatch(async |sent| {
            assert_eq!(sent.target.upload_id, UPLOAD);
            let composite = STANDARD.encode(Sha256::digest(part_digest));
            Ok(CompletionResponse {
                configuration,
                status: 200,
                version: Some(MEDIA.into()),
                body: format!(
                    "<CompleteMultipartUploadResult><Bucket>{BUCKET}</Bucket><Key>{key}</Key>
                    <ChecksumSHA256>{composite}-1</ChecksumSHA256></CompleteMultipartUploadResult>"
                )
                .into_bytes(),
            })
        })
        .await
        .unwrap();
    let put = SidecarPut::new(BUCKET, &context, &origin, map, 0, &page).unwrap();
    let page_key = put.request().0.key.clone();
    let sidecars = [put
        .verify_reply(SidecarPutReply {
            configuration,
            bucket: BUCKET,
            key: &page_key,
            status: 200,
            body: &[],
            version_headers: &[PAGE],
        })
        .unwrap()];
    let custody = ReadbackScope {
        bucket: BUCKET,
        configuration,
    };
    let inventory = CompletedReadbackInventory::assemble(
        BUCKET,
        configuration,
        &context,
        map,
        &origin,
        &completed,
        &sidecars,
    )
    .unwrap();
    let envelope = EnrolledEnvelope {
        authorized_root: &root,
        context: &context,
        trusted_record: &record,
        retained_record: &record,
    };
    let verified = inventory
        .verify(custody, envelope, async |_, pin| {
            let (version, data) = if pin.object_key == key {
                (MEDIA, &ciphertext)
            } else {
                assert_eq!(pin.object_key, page_key);
                (PAGE, &page)
            };
            assert_eq!(pin.version, Some(version));
            Ok(BucketReadbackResponse {
                bucket: BUCKET.into(),
                response: ReadbackResponse {
                    configuration,
                    object_key: pin.object_key.into(),
                    version: Some(version.into()),
                    status: 200,
                    content_length: Some(data.len() as u64),
                    has_content_range: false,
                    has_content_encoding: false,
                    body: Body(data),
                },
            })
        })
        .await
        .unwrap();
    if let Some(fault) = fault {
        let (auth, retain, expected) = match fault {
            Fault::Digest => (auth, now as i64 + 600, UploadError::Refused),
            Fault::Revoked => (
                Authorization {
                    record: &revoked,
                    ..auth
                },
                now as i64 + 600,
                UploadError::Refused,
            ),
            Fault::Retention => (auth, now as i64 - 1, UploadError::Unconfirmed),
        };
        assert!(
            matches!(opened.commit(&mut writer, auth, &verified, retain).await,
            Err(error) if error == expected)
        );
        let untouched: bool = observer.query_one(
            "SELECT terminal_reason IS NULL AND completed_map_id IS NULL AND write_lease_id IS NOT NULL
                AND checkpoint_revision = 0 AND checkpoint_offset = 0
             FROM public.ingest_session_reservations WHERE id = $1::text::uuid",
            &[&sid],
        ).await.unwrap().get(0);
        assert!(untouched);
        assert!(!observer.query_one(
            "SELECT EXISTS (SELECT FROM public.storage_digest_inventory_intents WHERE map_id = $1::text::uuid)
                 OR EXISTS (SELECT FROM public.storage_validated_digest_winners WHERE object_id = $2::text::uuid)",
            &[&map_text, &obj],
        ).await.unwrap().get::<_, bool>(0));
        return;
    }
    let retain = now as i64 + 600;
    let committed = opened
        .commit(&mut writer, auth, &verified, retain)
        .await
        .unwrap();
    assert_eq!(
        (&committed.media_version[..], &committed.page_versions[..]),
        (MEDIA, &[PAGE.to_string()][..])
    );
    assert_eq!(committed.map.replace('-', ""), map_text);
    let replayed = read(&mut writer, auth, prepared).await.unwrap().unwrap();
    assert_eq!(
        (replayed.map, replayed.media_version, replayed.page_versions),
        (
            committed.map,
            committed.media_version,
            committed.page_versions
        )
    );
    assert!(matches!(
        read(&mut writer, foreign, other).await,
        Err(UploadError::Refused)
    ));
    assert!(matches!(
        open(
            &mut writer,
            auth,
            prepared,
            until,
            async |_, _, _, _| panic!("completed retry reached the provider")
        )
        .await,
        Err(UploadError::Refused)
    ));
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
