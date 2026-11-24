use super::*;
use crate::multipart::Plan;
use tokio_postgres::NoTls;

const DATABASE: &str = "postgresql://postgres:ci-throwaway@127.0.0.1:5432/nntune_ci";
const BUCKET: &str = "handoff-fixture";
const PROVIDER_ID: &str = "fixture+/=?&opaque";
const OWNER: &str = "synthetic-handoff-owner";
const LEASE: &str = "61000000-0000-4000-8000-000000000001";
const NEXT_LEASE: &str = "61000000-0000-4000-8000-000000000002";

fn id(value: u8) -> [u8; 16] {
    let mut id = [value; 16];
    id[6] = 0x40;
    id[8] = 0x80;
    id
}

async fn connect() -> Client {
    let (client, connection) = tokio_postgres::connect(DATABASE, NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    client
}

async fn no_initiation<'a>(_: SystemTime) -> Result<InitiatedUpload<'a>, ()> {
    panic!("a second initiation was attempted")
}

async fn create_session(observer: &Client, context: &EnvelopeContext, session: &str) {
    let [_, environment, library, _, generation, _] = context.identities.map(|v| uuid(&v));
    observer
        .batch_execute(&format!(
            "SELECT pg_temp.reserve('{environment}', '{library}', '{OWNER}', '{session}',
                                    decode(repeat('af',32), 'hex'), '{session}');
             SELECT pg_temp.lease('{environment}', '{library}', '{OWNER}', '{session}',
                                  decode(repeat('af',32), 'hex'), '{generation}', 0, 0, '{LEASE}');"
        ))
        .await
        .unwrap();
}

async fn assert_claim(observer: &Client, session: &str, object: &str, expected: Option<&str>) {
    let row = observer
        .query_one(
            "SELECT multipart_object_id = $2::text::uuid AND multipart_bucket = $3, multipart_upload_id
             FROM public.ingest_session_reservations WHERE id = $1::text::uuid",
            &[&session, &object, &BUCKET],
        )
        .await
        .unwrap();
    assert!(row.get::<_, bool>(0));
    assert_eq!(row.get::<_, Option<&str>>(1), expected);
}

fn synthetic_receipt<'a>(
    context: &EnvelopeContext,
    key: &'a str,
    xml_upload_id: &str,
) -> InitiatedUpload<'a> {
    InitiatedUpload::verify(
        context.identities[5],
        BUCKET,
        key,
        200,
        format!(
            "<InitiateMultipartUploadResult><Bucket>{BUCKET}</Bucket><Key>{key}</Key>\
             <UploadId>{xml_upload_id}</UploadId></InitiateMultipartUploadResult>"
        )
        .as_bytes(),
    )
    .unwrap()
}

#[tokio::test]
#[ignore = "requires a migrated disposable PostgreSQL database"]
async fn postgres_fences_the_provider_id_handoff() {
    assert!(
        std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
            && std::env::var("RUNNER_ENVIRONMENT").as_deref() == Ok("github-hosted")
            && std::env::var("NNTUNE_TEST_DATABASE_URL").as_deref() == Ok(DATABASE)
    );
    let observer = connect().await;
    let mut writer = connect().await;
    let mut contender = connect().await;
    observer
        .batch_execute(include_str!("../../../api/src/ingest_session_schema.sql"))
        .await
        .unwrap();
    for (signature, returns, sql) in [
        (
            "reserve(uuid, uuid, text, text, bytea, uuid)",
            "uuid",
            include_str!("../../../api/src/ingest_session_reserve.sql"),
        ),
        (
            "lease(uuid, uuid, text, uuid, bytea, uuid, bigint, bigint, uuid)",
            "uuid",
            include_str!("../../../api/src/ingest_session_acquire_write_lease.sql"),
        ),
        (
            "advance(uuid, uuid, text, uuid, bytea, uuid, bigint, bigint, bigint, uuid)",
            "bigint",
            include_str!("../../../api/src/ingest_session_advance_checkpoint.sql"),
        ),
        (
            "terminate(uuid, uuid, text, uuid, text, bytea, uuid, bigint, bigint, uuid)",
            "text",
            include_str!("../../../api/src/ingest_session_terminate.sql"),
        ),
    ] {
        observer
            .batch_execute(&format!(
                "CREATE FUNCTION pg_temp.{signature} RETURNS SETOF {returns}
                 LANGUAGE sql VOLATILE AS $sql${sql}$sql$;"
            ))
            .await
            .unwrap();
    }
    let mut context = EnvelopeContext {
        identities: [0xb0, 0xb1, 0xb2, 0xb3, 0xb4, 0xb5].map(id),
        plaintext_len: 1,
        header: [17; 48],
    };
    let [_, environment, library, object, generation, configuration] =
        context.identities.map(|v| uuid(&v));
    let session = "61000000-0000-4000-8000-000000000003";
    let other_session = "61000000-0000-4000-8000-000000000004";
    let interrupted = "61000000-0000-4000-8000-000000000005";
    observer.batch_execute(&format!(
        "INSERT INTO public.users (id, email, created_at) VALUES ('{OWNER}', '{OWNER}@example.invalid', 1);
         INSERT INTO public.storage_environments VALUES ('{environment}');
         INSERT INTO public.storage_library_namespaces VALUES ('{library}', '{environment}', NULL);
         INSERT INTO public.storage_object_intents VALUES ('{object}', '{generation}', '{environment}',
             '{library}', '{configuration}', 1, 65, decode(repeat('11',48), 'hex'));
         CREATE FUNCTION public.fixture_stop_id_commit() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN RAISE EXCEPTION 'synthetic interruption at COMMIT'; END; $$;
         CREATE CONSTRAINT TRIGGER fixture_stop_id_commit AFTER UPDATE ON public.ingest_session_reservations
           DEFERRABLE INITIALLY DEFERRED FOR EACH ROW WHEN (NEW.multipart_upload_id = 'uncommitted')
           EXECUTE FUNCTION public.fixture_stop_id_commit();"
    )).await.unwrap();
    create_session(&observer, &context, session).await;
    create_session(&observer, &context, other_session).await;
    let scope = CompletionScope {
        environment: context.identities[1],
        library: context.identities[2],
        principal: OWNER,
        session,
        fingerprint: &[0xaf; 32],
        seal: context.identities[4],
        revision: 0,
        offset: 0,
    };
    let key = format!("media/{object}/{generation}");
    let until = SystemTime::now() + Duration::from_secs(60);
    let upload = handoff(
        &mut writer,
        &scope,
        LEASE,
        &context,
        (BUCKET, &key),
        until,
        async |deadline| {
            assert_eq!(deadline, until);
            assert_claim(&observer, session, &object, None).await;
            let competing = handoff(
                &mut contender,
                &scope,
                LEASE,
                &context,
                (BUCKET, &key),
                until,
                no_initiation,
            )
            .await;
            assert!(matches!(competing, Err(Pending)));
            let other = CompletionScope {
                session: other_session,
                ..scope
            };
            assert!(matches!(
                handoff(
                    &mut contender,
                    &other,
                    LEASE,
                    &context,
                    (BUCKET, &key),
                    until,
                    no_initiation
                )
                .await,
                Err(Refused)
            ));
            let advanced = observer
                .query_opt(
                    &format!(
                "SELECT pg_temp.advance('{environment}', '{library}', '{OWNER}', '{session}',
                 decode(repeat('af',32), 'hex'), '{generation}', 0, 0, 1, '{LEASE}')"
            ),
                    &[],
                )
                .await
                .unwrap();
            assert!(advanced.is_none());
            Ok(synthetic_receipt(&context, &key, "fixture+/=?&amp;opaque"))
        },
    )
    .await
    .unwrap();
    // A separate connection sees the ID before the first part can use it.
    assert_claim(&observer, session, &object, Some(PROVIDER_ID)).await;
    assert_eq!(upload.target().upload_id, PROVIDER_ID);
    assert_eq!(upload.usable_until(), until);
    let binding = ResumeBinding {
        plan: Plan {
            target: upload.target(),
            header: context.header,
            plaintext_len: 1,
            ciphertext_len: 65,
        },
        configuration: context.identities[5],
        environment: &environment,
        library: &library,
        object: &object,
        generation: &generation,
        session,
        envelope_scheme: 2,
    };
    assert!(upload.matches(binding, &context));
    assert!(!upload.matches(
        ResumeBinding {
            session: other_session,
            ..binding
        },
        &context
    ));
    drop(writer);
    let mut writer = connect().await;
    let replay = handoff(
        &mut writer,
        &scope,
        LEASE,
        &context,
        (BUCKET, &key),
        until,
        no_initiation,
    )
    .await
    .unwrap();
    assert_eq!(replay.target().upload_id, PROVIDER_ID);
    for (scope, bucket) in [
        (
            CompletionScope {
                principal: "foreign",
                ..scope
            },
            BUCKET,
        ),
        (
            CompletionScope {
                fingerprint: &[0x50; 32],
                ..scope
            },
            BUCKET,
        ),
        (
            CompletionScope {
                revision: 1,
                ..scope
            },
            BUCKET,
        ),
        (scope, "wrong-bucket"),
    ] {
        assert!(matches!(
            handoff(
                &mut contender,
                &scope,
                LEASE,
                &context,
                (bucket, &key),
                until,
                no_initiation
            )
            .await,
            Err(Refused)
        ));
    }
    observer
        .batch_execute(&format!(
            "SELECT pg_temp.advance('{environment}', '{library}', '{OWNER}', '{session}',
             decode(repeat('af',32), 'hex'), '{generation}', 0, 0, 1, '{LEASE}');
         SELECT pg_temp.lease('{environment}', '{library}', '{OWNER}', '{session}',
             decode(repeat('af',32), 'hex'), '{generation}', 1, 1, '{NEXT_LEASE}');"
        ))
        .await
        .unwrap();
    let progressed = CompletionScope {
        revision: 1,
        offset: 1,
        ..scope
    };
    assert!(matches!(
        handoff(
            &mut writer,
            &progressed,
            LEASE,
            &context,
            (BUCKET, &key),
            until,
            no_initiation
        )
        .await,
        Err(Refused)
    ));
    let replay = handoff(
        &mut writer,
        &progressed,
        NEXT_LEASE,
        &context,
        (BUCKET, &key),
        until,
        no_initiation,
    )
    .await
    .unwrap();
    assert_eq!(replay.target().upload_id, PROVIDER_ID);

    context.identities[3] = id(0xb6);
    context.identities[4] = id(0xb7);
    let object = uuid(&context.identities[3]);
    let generation = uuid(&context.identities[4]);
    let key = format!("media/{object}/{generation}");
    observer.batch_execute(&format!(
        "INSERT INTO public.storage_object_intents VALUES ('{object}', '{generation}', '{environment}',
             '{library}', '{configuration}', 1, 65, decode(repeat('11',48), 'hex'));"
    )).await.unwrap();
    create_session(&observer, &context, interrupted).await;
    let scope = CompletionScope {
        session: interrupted,
        seal: context.identities[4],
        ..scope
    };
    let lost = handoff(
        &mut writer,
        &scope,
        LEASE,
        &context,
        (BUCKET, &key),
        until,
        async |_| {
            assert_claim(&observer, interrupted, &object, None).await;
            Ok(synthetic_receipt(&context, &key, "uncommitted"))
        },
    )
    .await;
    assert!(matches!(lost, Err(Indeterminate)));
    assert_claim(&observer, interrupted, &object, None).await;
    assert!(matches!(
        handoff(
            &mut contender,
            &scope,
            LEASE,
            &context,
            (BUCKET, &key),
            until,
            no_initiation
        )
        .await,
        Err(Pending)
    ));
    let terminal = observer
        .query_one(
            &format!(
                "SELECT pg_temp.terminate('{environment}', '{library}', '{OWNER}', '{interrupted}',
             'checkpoint_lost', decode(repeat('af',32), 'hex'), '{generation}', 0, 0, '{LEASE}')"
            ),
            &[],
        )
        .await
        .unwrap();
    assert_eq!(terminal.get::<_, &str>(0), "aborted");
    assert_claim(&observer, interrupted, &object, None).await;
    assert!(observer
        .query_opt(
            "SELECT 1 FROM public.ingest_session_cleanup_intents
         WHERE session_id = $1::text::uuid AND kind = 'terminal_reconcile'",
            &[&interrupted],
        )
        .await
        .unwrap()
        .is_some());
    assert!(matches!(
        handoff(
            &mut writer,
            &scope,
            LEASE,
            &context,
            (BUCKET, &key),
            until,
            no_initiation
        )
        .await,
        Err(Refused)
    ));
    observer
        .batch_execute(
            "DROP TABLE public.ingest_session_cleanup_intents, public.ingest_session_reservations;
         DROP FUNCTION public.fixture_stop_id_commit();",
        )
        .await
        .unwrap();
}
