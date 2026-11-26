use super::*;
use crate::handlers::private_trial::{registered::registered_header, TrialError};
use crate::native_rest::{issue_credential, Scope};
use base64::{engine::general_purpose::STANDARD, Engine};
use nntune_core::{
    digest_enrollment::DigestEnrollment,
    digest_readback::{
        BodyReadbackError, EnrolledEnvelope, EnrolledReadbackError, ReadbackBody, ReadbackError,
        ReadbackPin, ReadbackResponse,
    },
    media::PLAIN_CHUNK_SIZE,
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
    Plan, UploadTarget,
};
use sha2::{Digest, Sha256};
use std::{io, net::Ipv4Addr};
use tokio_postgres::{Config, NoTls};

#[path = "../support/blocker_observer.rs"]
mod blocker_observer;
use blocker_observer::observe_blocked;

#[path = "ingest_session_reuse_postgres.rs"]
mod reuse_cases;

const DB: &str = "postgresql://postgres:ci-throwaway@127.0.0.1:5432/nntune_upload_caller_ci";
const WRITER: &str =
    "postgresql://upload_fixture_writer:ci-throwaway@127.0.0.1:5432/nntune_upload_caller_ci";
const AUTHORITY: &str =
    "postgresql://upload_fixture_authority:ci-throwaway@127.0.0.1:5432/nntune_upload_caller_ci";
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

async fn load_authority(
    authority: &Client,
    credential_id: &str,
    scope: &CompletionScope<'_>,
) -> Result<Option<CurrentAuthorization>, StoreError> {
    let row = authority.query_opt(
        "SELECT c.*, g.environment, g.library_id, g.can_reuse
         FROM public.native_rest_credentials c JOIN public.fixture_upload_grants g ON g.credential_id = c.id
         WHERE c.id = $1 AND c.user_id = $2 AND g.environment = $3 AND g.library_id = $4",
        &[&credential_id, &scope.principal, &uuid_text(scope.environment), &uuid_text(scope.library)],
    ).await.map_err(|_| StoreError)?;
    row.map(|row| {
        let record = crate::worker_entry::native_credential_from_row(&row)?.record;
        let mut actions = vec![Action::Upload];
        if row.try_get("can_reuse").map_err(|_| StoreError)? {
            actions.push(Action::Reuse);
        }
        let grant = Grant {
            credential_id: record.id.clone(),
            environment: row.try_get("environment").map_err(|_| StoreError)?,
            library_id: row.try_get("library_id").map_err(|_| StoreError)?,
            actions,
        };
        Ok(CurrentAuthorization { record, grant })
    })
    .transpose()
}

#[test]
#[ignore = "requires the disposable hosted upload caller database"]
fn scoped_writer_commits_and_replays_exact_upload() {
    // The combined upload and publication futures exceed libtest's default stack in debug builds.
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(prove_scoped_writer());
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn prove_scoped_writer() {
    let observer = connect(DB).await;
    observer
        .batch_execute(include_str!("ingest_session_writer_grants.sql"))
        .await
        .unwrap();
    observer
        .batch_execute(include_str!("ingest_session_caller_grants.sql"))
        .await
        .unwrap();
    observer
        .batch_execute(include_str!("ingest_session_reuse_grants.sql"))
        .await
        .unwrap();
    observer.batch_execute(
        "CREATE ROLE upload_fixture_authority LOGIN NOINHERIT PASSWORD 'ci-throwaway';
         CREATE TABLE public.fixture_upload_grants (
             credential_id text PRIMARY KEY REFERENCES public.native_rest_credentials (id),
             environment text NOT NULL, library_id text NOT NULL, can_reuse boolean NOT NULL DEFAULT true
         );
         GRANT SELECT ON public.native_rest_credentials, public.fixture_upload_grants TO upload_fixture_authority;
         CREATE FUNCTION public.fixture_hold_completion() RETURNS trigger
         LANGUAGE plpgsql SET search_path = pg_catalog AS $$
         BEGIN
             PERFORM pg_advisory_xact_lock(1751);
             RETURN NEW;
         END $$;
         CREATE CONSTRAINT TRIGGER fixture_hold_completion
         AFTER UPDATE ON public.ingest_session_reservations DEFERRABLE INITIALLY DEFERRED
         FOR EACH ROW WHEN (NEW.terminal_reason = 'completed')
         EXECUTE FUNCTION public.fixture_hold_completion();",
    ).await.unwrap();
    let authority = connect(AUTHORITY).await;
    let mut writer = connect(WRITER).await;
    assert!(writer.query_one(
        "SELECT session_user = current_user AND current_user = 'upload_fixture_writer'
            AND NOT EXISTS (SELECT FROM pg_auth_members WHERE member = current_user::regrole)
            AND NOT (SELECT rolinherit OR rolsuper FROM pg_roles WHERE rolname = current_user)
            AND NOT has_table_privilege(current_user, 'public.ingest_session_reservations', 'DELETE,TRUNCATE')
            AND NOT has_any_column_privilege(current_user, 'public.native_rest_credentials', 'SELECT,INSERT,UPDATE')
            AND NOT has_table_privilege(current_user, 'public.native_rest_credentials', 'DELETE,TRUNCATE')
            AND NOT has_any_column_privilege(current_user, 'public.fixture_upload_grants', 'SELECT,INSERT,UPDATE')
            AND NOT has_table_privilege(current_user, 'public.fixture_upload_grants', 'DELETE,TRUNCATE')
            AND NOT has_any_column_privilege(current_user, 'public.track_sources', 'INSERT,UPDATE')
            AND NOT has_any_column_privilege(current_user, 'public.storage_validated_digest_references', 'INSERT,UPDATE')
            AND NOT has_table_privilege(current_user, 'public.track_sources', 'DELETE,TRUNCATE')",
        &[],
    ).await.unwrap().get::<_, bool>(0));

    for (index, fault) in [
        None,
        Some(Fault::GrantRevokedWhileWaiting),
        Some(Fault::Digest),
        Some(Fault::Revoked),
        Some(Fault::Retention),
        Some(Fault::BadFlac),
        Some(Fault::SidecarVersion),
        Some(Fault::MissingSeek),
        Some(Fault::ForeignTarget),
        Some(Fault::CommittedSidecarVersion),
        Some(Fault::Quarantined),
        Some(Fault::LostCompletionResponse),
    ]
    .into_iter()
    .enumerate()
    {
        prove_attempt(
            &observer,
            &authority,
            &mut writer,
            index as u8 * 16,
            fault,
            false,
        )
        .await;
    }
    // Reuse tests tombstone their winner. Publication needs its own live winner
    // and creates a reference that must block deletion in the reuse scenario.
    prove_attempt(&observer, &authority, &mut writer, 0xc0, None, true).await;
}

#[derive(Clone, Copy)]
enum Fault {
    GrantRevokedWhileWaiting,
    Digest,
    Revoked,
    Retention,
    BadFlac,
    SidecarVersion,
    MissingSeek,
    ForeignTarget,
    CommittedSidecarVersion,
    Quarantined,
    LostCompletionResponse,
}

async fn prove_attempt(
    observer: &Client,
    authority: &Client,
    mut writer: &mut Client,
    seed: u8,
    fault: Option<Fault>,
    publish_fixture: bool,
) {
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
    let input = if publish_fixture {
        include_bytes!("../../../../fixtures/synthetic-flac/private-staging/tone-a.flac").as_slice()
    } else {
        FLAC
    };
    let length = input.len() as u64;
    let mut sealer = DigestEnrollment::new(length, identities, map).unwrap();
    let header = sealer.header();
    let context = EnvelopeContext {
        identities: [root_id, environment, library, object, seal, configuration],
        plaintext_len: length,
        header: header.to_bytes(),
    };
    let root = [17; 32];
    let record = wrap_scheme2(&root, &context, sealer.key().expose_secret()).unwrap();
    let mut plaintext = input.to_vec();
    if matches!(fault, Some(Fault::BadFlac)) {
        *plaintext.last_mut().unwrap() ^= 1;
    }
    let sealed: Vec<_> = plaintext
        .chunks(PLAIN_CHUNK_SIZE as usize)
        .map(|chunk| sealer.seal_chunk(chunk).unwrap())
        .collect();
    let page = sealed
        .iter()
        .filter_map(|chunk| chunk.page.as_ref())
        .next()
        .unwrap();
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
         INSERT INTO public.storage_library_namespaces VALUES ('{lib}', '{env}', NULL);
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
        credential_id: credential.record.id.clone(),
        environment: env.clone(),
        library_id: lib.clone(),
        actions: vec![Action::Create, Action::Upload, Action::Reuse],
    };
    let credential_record = &credential.record;
    observer.execute(
        "INSERT INTO public.native_rest_credentials
            (id, user_id, key_prefix, key_hmac, pepper_version, scopes, created_at, expires_at, created_from_ip)
         VALUES ($1, $2, $3, $4, 1, ARRAY['ingest:submit'], $5, $6, '127.0.0.1')",
        &[&credential_record.id, &credential_record.user_id, &credential_record.key_prefix, &credential_record.key_hmac,
          &(credential_record.created_at as i64), &(credential_record.expires_at as i64)],
    ).await.unwrap();
    observer
        .execute(
            "INSERT INTO public.fixture_upload_grants (credential_id, environment, library_id) VALUES ($1, $2, $3)",
            &[&credential_record.id, &env, &lib],
        )
        .await
        .unwrap();
    let reload =
        async |id: &str, scope: &CompletionScope<'_>| load_authority(authority, id, scope).await;
    let auth = Authorization {
        record: credential_record,
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
    let idempotency_key = if publish_fixture {
        crate::handlers::tests::private_trial_fixture::JOB
    } else {
        "fixture-key"
    };
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
        &[&env, &lib, &OWNER, &idempotency_key, &fingerprint.bytes().as_slice(), &sid],
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
    if publish_fixture {
        assert_eq!(
            registered_header(writer, &prepared.scope, identities, map, idempotency_key).await,
            Err(TrialError::Unconfirmed)
        );
    }

    let mut stranger = credential.record.clone();
    stranger.user_id = "foreign-principal".into();
    let foreign = Authorization {
        record: &stranger,
        ..auth
    };
    let mut revoked = credential.record.clone();
    revoked.revoked_at = Some(now);
    let wrong_environment = Grant {
        environment: "foreign".into(),
        ..grant.clone()
    };
    let wrong_library = Grant {
        library_id: "foreign".into(),
        ..grant.clone()
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
            read(writer, wrong, prepared, reload).await,
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

    assert!(matches!(
        open_or_reuse(
            &mut writer,
            auth,
            prepared,
            UNIX_EPOCH,
            async |_, _| panic!("a fresh session has no verified result to authorize"),
            async |_, _, _, _| panic!("an expired upload must not reach the provider"),
        )
        .await,
        Err(UploadError::Refused)
    ));
    let UploadAttempt::Opened(opened) =
        open_or_reuse(
            &mut writer,
            auth,
            prepared,
            until,
            async |_, _| panic!("a fresh session has no verified result to authorize"),
            async |db, scope, lease, deadline| {
                assert_eq!(deadline, until);
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
        .unwrap()
    else {
        panic!("a fresh reservation must open a new upload");
    };
    let target = opened.durable_target().target();
    assert_eq!(target.upload_id, UPLOAD);
    let row = observer.query_one(
        "SELECT multipart_upload_id FROM public.ingest_session_reservations WHERE id = $1::text::uuid",
        &[&sid],
    ).await.unwrap();
    assert_eq!(row.get::<_, &str>(0), UPLOAD);
    assert!(matches!(
        open_or_reuse(
            &mut writer,
            auth,
            prepared,
            until,
            async |_, _| panic!("an in-flight upload has no reusable result"),
            async |_, _, _, _| panic!("open replay reached the provider")
        )
        .await,
        Err(UploadError::Refused)
    ));

    let inventory_bucket = if matches!(fault, Some(Fault::ForeignTarget)) {
        "foreign-fixture-bucket"
    } else {
        BUCKET
    };
    let plan = Plan::new(
        UploadTarget {
            bucket: inventory_bucket,
            ..target
        },
        header,
        length,
    )
    .unwrap();
    let mut partition = plan.partition();
    for (index, chunk) in sealed.iter().enumerate() {
        let part = partition
            .push(&SealedChunk {
                digest: Sha256::digest(&chunk.bytes).into(),
                bytes: chunk.bytes.clone(),
                final_chunk: index + 1 == sealed.len(),
            })
            .unwrap();
        assert!(part.is_none());
    }
    let part = partition.finish().unwrap().unwrap();
    let (_, number, ciphertext, checksum) = part.request();
    assert_ne!(ciphertext, input);
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
                    "<CompleteMultipartUploadResult><Bucket>{inventory_bucket}</Bucket><Key>{key}</Key>
                    <ChecksumSHA256>{composite}-1</ChecksumSHA256></CompleteMultipartUploadResult>"
                )
                .into_bytes(),
            })
        })
        .await
        .unwrap();
    let put = SidecarPut::new(inventory_bucket, &context, &origin, map, 0, page).unwrap();
    let page_key = put.request().0.key.clone();
    let sidecars = [put
        .verify_reply(SidecarPutReply {
            configuration,
            bucket: inventory_bucket,
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
        inventory_bucket,
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
    let expected_deadline = opened.durable_target().usable_until();
    let verify = async |deadline| {
        assert!(
            !matches!(fault, Some(Fault::ForeignTarget)),
            "foreign inventory reached readback"
        );
        assert_eq!(deadline, expected_deadline);
        let open = async |_: &str, pin: ReadbackPin<'_>| {
            let (version, data) = if pin.object_key == key {
                (MEDIA, &ciphertext)
            } else {
                assert_eq!(pin.object_key, page_key);
                (PAGE, page)
            };
            assert_eq!(pin.version, Some(version));
            let returned_version =
                if version == PAGE && matches!(fault, Some(Fault::SidecarVersion)) {
                    "foreign-page-version"
                } else {
                    version
                };
            Ok(BucketReadbackResponse {
                bucket: BUCKET.into(),
                response: ReadbackResponse {
                    configuration,
                    object_key: pin.object_key.into(),
                    version: Some(returned_version.into()),
                    status: 200,
                    content_length: Some(data.len() as u64),
                    has_content_range: false,
                    has_content_encoding: false,
                    body: Body(data),
                },
            })
        };
        let verified = if matches!(fault, Some(Fault::MissingSeek)) {
            inventory.verify(custody, envelope, open).await
        } else {
            inventory
                .verify_with_flac_seek(custody, envelope, open)
                .await
        };
        match fault {
            Some(Fault::BadFlac) => assert!(matches!(
                verified,
                Err(EnrolledReadbackError::Body(BodyReadbackError::Content(
                    ReadbackError::MediaValidation
                )))
            )),
            Some(Fault::SidecarVersion) => assert!(matches!(
                verified,
                Err(EnrolledReadbackError::Body(BodyReadbackError::Response))
            )),
            _ => assert!(verified.is_ok()),
        }
        verified
    };
    if matches!(fault, Some(Fault::LostCompletionResponse)) {
        // Model the already charged admission using the real ingest job writer.
        let accepted_at = now as i64;
        assert_eq!(
            observer
                .execute(
                    nntune_ingest::ingest_jobs::upsert_ingest_job_sql(),
                    &[&sid, &OWNER, &idempotency_key, &accepted_at],
                )
                .await
                .unwrap(),
            1
        );
        let quota_used = async || {
            observer
                .query_one(
                    "SELECT count(*) FROM public.ingest_jobs
                     WHERE user_id = $1 AND accepted_at / 86400 = $2::bigint / 86400
                       AND source = 'pipeline'",
                    &[&OWNER, &accepted_at],
                )
                .await
                .unwrap()
                .get::<_, i64>(0)
        };
        assert_eq!(quota_used().await, 1);

        // The caller can lose its result when the post-commit read fails.
        let result = Box::pin(opened.commit_flac(
            writer,
            auth,
            &inventory,
            accepted_at + 600,
            async |_, _| Err(StoreError),
            verify,
        ))
        .await;
        assert!(matches!(result, Err(UploadError::Unconfirmed)));
        let final_scope = prepared.completed_scope();
        // The row versions expose a second write even if it repeats the same values.
        let committed_rows = async || {
            let row = observer
                .query_one(
                    "SELECT s.xmin::text, i.xmin::text
                     FROM public.ingest_session_reservations s
                     JOIN public.storage_digest_inventory_intents i ON i.map_id = s.completed_map_id
                     WHERE s.id = $1::text::uuid AND s.terminal_reason = 'completed'
                       AND s.checkpoint_generation = $2::text::uuid AND s.checkpoint_revision = $3
                       AND s.checkpoint_offset = $4 AND s.write_lease_id IS NULL
                       AND i.map_id = $5::text::uuid",
                    &[
                        &sid,
                        &generation,
                        &final_scope.revision,
                        &final_scope.offset,
                        &map_text,
                    ],
                )
                .await
                .expect("the first completion and its receipt must already be committed");
            (row.get::<_, String>(0), row.get::<_, String>(1))
        };
        let committed = committed_rows().await;

        let mut retry_writer = connect(WRITER).await;
        let UploadAttempt::Reused(replayed) = Box::pin(open_or_reuse(
            &mut retry_writer,
            auth,
            prepared,
            until,
            reload,
            async |_, _, _, _| panic!("completion replay reached the upload provider"),
        ))
        .await
        .unwrap() else {
            panic!("the same generation must replay the committed upload");
        };
        let receipt = replayed.outcome();
        assert_eq!(receipt.map.replace('-', ""), map_text);
        assert_eq!(receipt.object.replace('-', ""), obj);
        assert_eq!(receipt.generation.replace('-', ""), generation);
        assert_eq!(receipt.configuration.replace('-', ""), provider);
        assert_eq!(receipt.root_version.replace('-', ""), root_text);
        assert_eq!(receipt.media_version, MEDIA);
        assert_eq!(receipt.page_versions, [PAGE]);
        assert_eq!(committed_rows().await, committed);
        assert_eq!(quota_used().await, 1);
        return;
    }
    if matches!(fault, Some(Fault::CommittedSidecarVersion)) {
        // Inject a different version only after the real commit. Only the
        // disposable database owner can bypass the append-only fixture guard.
        const GUARD: &str = "storage_digest_inventory_intent_append_only";
        const TABLE: &str = "public.storage_digest_inventory_intents";
        const OTHER_PAGE: &str = "different-committed-sidecar-version";
        observer
            .batch_execute(&format!("ALTER TABLE {TABLE} DISABLE TRIGGER {GUARD}"))
            .await
            .unwrap();
        let reload_with_mismatch = async |id: &str, scope: &CompletionScope<'_>| {
            assert_eq!(observer.execute(
                "UPDATE public.storage_digest_inventory_intents p SET page_versions = ARRAY[$1::text]
                 FROM public.ingest_session_reservations s
                 WHERE s.id = $2::text::uuid AND s.terminal_reason = 'completed'
                   AND p.map_id = s.completed_map_id AND p.map_id = $3::text::uuid
                   AND p.page_versions = ARRAY[$4::text]",
                &[&OTHER_PAGE, &sid, &map_text, &PAGE],
            ).await.unwrap(), 1);
            load_authority(authority, id, scope).await
        };
        let result = opened
            .commit_flac(
                writer,
                auth,
                &inventory,
                now as i64 + 600,
                reload_with_mismatch,
                verify,
            )
            .await;
        observer
            .batch_execute(&format!("ALTER TABLE {TABLE} ENABLE TRIGGER {GUARD}"))
            .await
            .unwrap();
        let stored = read(writer, auth, prepared, reload).await.unwrap().unwrap();
        assert_eq!(stored.page_versions, [OTHER_PAGE]);
        assert!(matches!(result, Err(UploadError::Unconfirmed)));
        return;
    }
    if matches!(fault, Some(Fault::Quarantined)) {
        // Make the winner unreadable only at the outer commit. The observer
        // owns this fixture trigger; the writer's grants stay unchanged.
        observer
            .batch_execute(&format!(
                "CREATE FUNCTION public.fixture_quarantine_at_commit() RETURNS trigger
             LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
             BEGIN
                 UPDATE public.storage_validated_digest_winners SET quarantined = true
                 WHERE object_id = '{obj}'::uuid;
                 RETURN NEW;
             END $$;
             REVOKE ALL ON FUNCTION public.fixture_quarantine_at_commit() FROM PUBLIC;
             CREATE CONSTRAINT TRIGGER fixture_quarantine_at_commit
             AFTER UPDATE ON public.ingest_session_reservations DEFERRABLE INITIALLY DEFERRED
             FOR EACH ROW WHEN (NEW.id = '{sid}'::uuid AND NEW.terminal_reason = 'completed')
             EXECUTE FUNCTION public.fixture_quarantine_at_commit();"
            ))
            .await
            .unwrap();
        assert!(matches!(
            opened
                .commit_flac(writer, auth, &inventory, now as i64 + 600, reload, verify)
                .await,
            Err(UploadError::Unconfirmed)
        ));
        let committed: bool = observer.query_one(
            "SELECT s.terminal_reason = 'completed' AND s.write_lease_id IS NULL
                    AND w.quarantined AND p.media_version = $4::text
             FROM public.ingest_session_reservations s
             JOIN public.storage_digest_inventory_intents p ON p.map_id = s.completed_map_id
             JOIN public.storage_validated_digest_winners w ON w.object_id = s.multipart_object_id
             WHERE s.id = $1::text::uuid AND p.map_id = $2::text::uuid AND w.object_id = $3::text::uuid",
            &[&sid, &map_text, &obj, &MEDIA],
        ).await.unwrap().get(0);
        assert!(committed);
        assert!(matches!(
            read(writer, auth, prepared, reload).await,
            Err(UploadError::Refused)
        ));
        return;
    }
    if let Some(fault) = fault.filter(|fault| !matches!(fault, Fault::GrantRevokedWhileWaiting)) {
        let (auth, retain, expected) = match fault {
            Fault::Digest | Fault::MissingSeek | Fault::ForeignTarget => {
                (auth, now as i64 + 600, UploadError::Refused)
            }
            Fault::Revoked => (
                Authorization {
                    record: &revoked,
                    ..auth
                },
                now as i64 + 600,
                UploadError::Refused,
            ),
            Fault::Retention => (auth, now as i64 - 1, UploadError::Unconfirmed),
            Fault::BadFlac | Fault::SidecarVersion => {
                (auth, now as i64 + 600, UploadError::Unconfirmed)
            }
            Fault::Quarantined
            | Fault::GrantRevokedWhileWaiting
            | Fault::CommittedSidecarVersion
            | Fault::LostCompletionResponse => unreachable!(),
        };
        assert!(
            matches!(opened.commit_flac(writer, auth, &inventory, retain, reload, verify).await,
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
        if matches!(fault, Fault::BadFlac) {
            reuse_cases::refuse_unverified(observer, writer, auth, prepared, &map_text).await;
        }
        return;
    }
    let revoke = matches!(fault, Some(Fault::GrantRevokedWhileWaiting));
    let (committed, replayed) = {
        // Pause the real commit while it holds the session row and has staged the outcome.
        let mut gate = connect(DB).await;
        let mut replay_writer = connect(WRITER).await;
        let writer_pid = backend_pid(writer).await;
        let replay_pid = backend_pid(&replay_writer).await;
        let gate_pid = backend_pid(&gate).await;
        let gate = gate.transaction().await.unwrap();
        gate.query_one("SELECT pg_advisory_xact_lock(1751)", &[])
            .await
            .unwrap();
        let mut committed = std::pin::pin!(opened.commit_flac(
            writer,
            auth,
            &inventory,
            now as i64 + 600,
            reload,
            verify
        ));
        observe_blocked(observer, gate_pid, writer_pid, committed.as_mut())
            .await
            .unwrap();
        let mut replayed = std::pin::pin!(read(&mut replay_writer, auth, prepared, reload));
        observe_blocked(observer, writer_pid, replay_pid, replayed.as_mut())
            .await
            .unwrap();
        if revoke {
            assert_eq!(
                observer
                    .execute(
                        "DELETE FROM public.fixture_upload_grants WHERE credential_id = $1",
                        &[&auth.record.id]
                    )
                    .await
                    .unwrap(),
                1
            );
        }
        gate.commit().await.unwrap();
        tokio::join!(committed, replayed)
    };
    if revoke {
        assert!(matches!(committed, Err(UploadError::Unconfirmed)));
        assert!(matches!(replayed, Err(UploadError::Refused)));
        assert!(observer
            .query_one(
                "SELECT s.terminal_reason = 'completed' AND s.write_lease_id IS NULL
                    AND p.media_version = $3 AND p.page_versions = $4
             FROM public.ingest_session_reservations s
             JOIN public.storage_digest_inventory_intents p ON p.map_id = s.completed_map_id
             WHERE s.id = $1::text::uuid AND p.map_id = $2::text::uuid",
                &[&sid, &map_text, &MEDIA, &&[PAGE][..]],
            )
            .await
            .unwrap()
            .get::<_, bool>(0));
        return;
    }
    let committed = committed.unwrap();
    // The staged fixture's next sparse seek anchor is at 5.015 seconds.
    let seek_range = if publish_fixture {
        8256..68861
    } else {
        42..length
    };
    assert_eq!(
        committed.flac_plaintext_range(completed.metadata(), origin.plaintext_sha256(), 0..1),
        Some(seek_range)
    );
    assert_eq!(
        committed.flac_plaintext_range(completed.metadata(), &[0; 32], 0..1),
        None
    );
    for outcome in [committed.outcome(), &replayed.unwrap().unwrap()] {
        assert_eq!(outcome.map.replace('-', ""), map_text);
        assert_eq!(outcome.object.replace('-', ""), obj);
        assert_eq!(outcome.generation.replace('-', ""), generation);
        assert_eq!(outcome.configuration.replace('-', ""), provider);
        assert_eq!(outcome.root_version.replace('-', ""), root_text);
        assert_eq!(outcome.media_version, MEDIA);
        assert_eq!(outcome.page_versions, [PAGE]);
    }
    assert!(matches!(
        read(writer, auth, prepared, async |_, _| Err(StoreError)).await,
        Err(UploadError::Unconfirmed)
    ));
    let stalled = tokio::time::timeout(
        Duration::from_secs(6),
        read(writer, auth, prepared, async |_, _| {
            std::future::pending().await
        }),
    )
    .await
    .expect("the authority deadline must end a stalled replay");
    assert!(matches!(stalled, Err(UploadError::Unconfirmed)));
    // Drain the rollback queued by the dropped transaction before checking its lock.
    writer.simple_query("SELECT 1").await.unwrap();
    observer.query_one(
        "SELECT 1 FROM public.ingest_session_reservations WHERE id = $1::text::uuid FOR UPDATE NOWAIT",
        &[&sid],
    ).await.unwrap();
    let mismatched = async |id: &str, scope: &CompletionScope<'_>| {
        let mut state = reload(id, scope).await?.unwrap();
        state.grant.library_id = "foreign".into();
        Ok(Some(state))
    };
    assert!(matches!(
        read(writer, auth, prepared, mismatched).await,
        Err(UploadError::Refused)
    ));
    assert!(matches!(
        read(writer, foreign, other, reload).await,
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
    let track = if publish_fixture {
        let header = async |scope: &CompletionScope<'_>, map, key: &str| {
            registered_header(writer, scope, identities, map, key).await
        };
        let scope = prepared.scope;
        assert_eq!(
            header(&scope, map, idempotency_key).await,
            Ok(Some(context.header))
        );
        assert_eq!(
            header(&scope, [0; 16], idempotency_key).await,
            Err(TrialError::Unconfirmed)
        );
        assert_eq!(header(&scope, map, "other-fixture").await, Ok(None));
        let foreign = CompletionScope {
            principal: "foreign-principal",
            ..scope
        };
        assert_eq!(header(&foreign, map, idempotency_key).await, Ok(None));
        let track = Box::pin(
            crate::handlers::tests::private_trial_fixture::verify_publication(
                observer, writer, now, input, auth, prepared, &committed, reload,
            ),
        )
        .await;
        Box::pin(prove_overlapping_completed_reads(
            observer,
            authority,
            writer,
            auth,
            prepared,
            committed.outcome(),
            &track,
        ))
        .await;
        crate::handlers::tests::private_trial_fixture::verify_registration(
            observer, authority, input, auth,
        )
        .await;
        Some(track)
    } else {
        None
    };
    observer
        .execute(
            "UPDATE public.native_rest_credentials SET revoked_at = $2 WHERE id = $1",
            &[&auth.record.id, &(now as i64)],
        )
        .await
        .unwrap();
    assert!(matches!(
        read(writer, auth, prepared, reload).await,
        Err(UploadError::Refused)
    ));
    if let Some(track) = track {
        Box::pin(
            crate::handlers::tests::private_trial_fixture::verify_cleanup(
                observer, prepared, &track, now,
            ),
        )
        .await;
        return;
    }
    // Restore the disposable credential for the independent reuse cases.
    observer
        .execute(
            "UPDATE public.native_rest_credentials SET revoked_at = NULL WHERE id = $1",
            &[&auth.record.id],
        )
        .await
        .unwrap();
    reuse_cases::protect_completed(observer, authority, writer, auth, prepared, &committed).await;
}

async fn backend_pid(client: &Client) -> i32 {
    client
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

async fn prove_overlapping_completed_reads(
    observer: &Client,
    authority: &Client,
    status_writer: &mut Client,
    auth: Authorization<'_>,
    prepared: PreparedUpload<'_>,
    acknowledged: &CommittedOutcome,
    track: &str,
) {
    // Compare upload and publication row versions to catch even same-value writes.
    let snapshot = async || {
        observer
            .query_one(
                "SELECT c.track_id::text, ARRAY[s.xmin::text, i.xmin::text, l.xmin::text,
                                          c.xmin::text, t.xmin::text, a.xmin::text, r.xmin::text]
             FROM public.ingest_session_reservations s
             JOIN public.storage_digest_inventory_intents i ON i.map_id = s.completed_map_id
             JOIN public.storage_s3_locator_intents l ON l.object_id = s.multipart_object_id
             JOIN public.catalogue_s3_sources c ON c.object_id = l.object_id
             JOIN public.tracks t ON t.id = c.track_id
             JOIN public.private_library_admissions a
                ON a.object_id = c.object_id AND a.track_id = c.track_id
             JOIN public.storage_validated_digest_references r
                ON r.object_id = c.object_id AND r.reference_id = c.track_id
             WHERE s.id = $1::text::uuid AND s.multipart_upload_id = $2
               AND s.terminal_reason = 'completed' AND s.write_lease_id IS NULL
               AND i.map_id = $3::text::uuid AND l.s3_version_id = i.media_version",
                &[&prepared.scope.session, &UPLOAD, &acknowledged.map],
            )
            .await
            .map(|row| (row.get::<_, String>(0), row.get::<_, Vec<String>>(1)))
            .unwrap()
    };
    let before = snapshot().await;
    assert_eq!(before.0, track);

    let mut resume_writer = connect(WRITER).await;
    let status_pid = backend_pid(status_writer).await;
    let resume_pid = backend_pid(&resume_writer).await;
    let reload =
        async |id: &str, scope: &CompletionScope<'_>| load_authority(authority, id, scope).await;
    let (status_locked, entered) = tokio::sync::oneshot::channel();
    let (release_status, released) = tokio::sync::oneshot::channel();
    let mut status = Box::pin(read(
        status_writer,
        auth,
        prepared,
        async move |id, scope| {
            status_locked.send(()).unwrap();
            released.await.unwrap();
            load_authority(authority, id, scope).await
        },
    ));
    // The status authority callback runs under the real scoped session lock.
    tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(2), entered) => {
            result.expect("status must reach authorization").unwrap();
        }
        _ = &mut status => panic!("status returned before resume could overlap"),
    }
    let mut resume = Box::pin(open_or_reuse(
        &mut resume_writer,
        auth,
        prepared,
        SystemTime::now() + Duration::from_secs(30),
        reload,
        async |_, _, _, _| panic!("resume attempted a second upload after completion"),
    ));
    observe_blocked(observer, status_pid, resume_pid, resume.as_mut())
        .await
        .expect("resume must wait for the completed session held by status");
    release_status.send(()).unwrap();
    let (status, resume) = tokio::join!(status, resume);
    let UploadAttempt::Reused(resume) = resume.expect("resume must return the committed upload")
    else {
        panic!("completed session opened a new upload");
    };
    for result in [&status.unwrap().unwrap(), resume.outcome()] {
        assert_eq!(result.map, acknowledged.map);
        assert_eq!(result.object, acknowledged.object);
        assert_eq!(result.generation, acknowledged.generation);
        assert_eq!(result.configuration, acknowledged.configuration);
        assert_eq!(result.root_version, acknowledged.root_version);
        assert_eq!(result.media_version, acknowledged.media_version);
        assert_eq!(result.page_versions, acknowledged.page_versions);
    }
    assert_eq!(snapshot().await, before);
}
