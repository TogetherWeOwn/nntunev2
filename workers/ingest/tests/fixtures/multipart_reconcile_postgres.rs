use super::*;
use crate::{
    digest_candidate_store::uuid,
    multipart::{session_outcome::CompletionScope, session_upload},
};
use nntune_core::media_envelope::EnvelopeContext;
use reconcile::dispatch_multipart_claims;
use std::{cell::Cell, future::pending, time::SystemTime};

const DB: &str = "postgresql://postgres:ci-throwaway@127.0.0.1:5432/nntune_ci";
const BUCKET: &str = "reconcile-fixture";
const UPLOAD: &str = "lost+/=?&upload";
const OWNER: &str = "synthetic-reconcile-owner";

async fn connect() -> Client {
    let (db, connection) = tokio_postgres::connect(DB, tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    db
}

fn deadline() -> tokio::time::Sleep {
    tokio::time::sleep(Duration::from_secs(10))
}

async fn unexpected_list(_: &str, _: SystemTime) -> Result<String, ()> {
    panic!("unexpected multipart listing")
}

async fn unexpected_abort(_: &str, _: &str, _: SystemTime) -> Result<(), ()> {
    panic!("unexpected multipart abort")
}

async fn unexpected_send(_: CleanupMessage, _: u32) -> Result<(), ()> {
    panic!("unconfirmed reconciliation reached queue")
}

async fn expire_claim(db: &Client, session: &str) {
    assert_eq!(
        db.execute(
            "UPDATE public.ingest_session_cleanup_intents
             SET claimed_until = clock_timestamp() - interval '1 second'
             WHERE session_id = $1::text::uuid AND kind = 'terminal_reconcile'
               AND delivery_claim_id IS NOT NULL AND enqueued_at IS NULL",
            &[&session],
        )
        .await
        .unwrap(),
        1
    );
}

async fn saved_id(db: &Client, session: &str) -> Option<String> {
    db.query_one(
        "SELECT multipart_upload_id FROM public.ingest_session_reservations WHERE id = $1::text::uuid",
        &[&session],
    ).await.unwrap().get(0)
}

#[tokio::test]
#[ignore = "requires the disposable hosted migration database"]
async fn postgres_reconciles_a_lost_initiation() {
    assert!(
        std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
            && std::env::var("RUNNER_ENVIRONMENT").as_deref() == Ok("github-hosted")
            && std::env::var("NNTUNE_TEST_DATABASE_URL").as_deref() == Ok(DB)
    );
    let mut writer = connect().await;
    let observer = connect().await;
    observer
        .batch_execute(include_str!("../../../api/src/ingest_session_schema.sql"))
        .await
        .unwrap();
    let mut identities = [[0; 16]; 6];
    for identity in &mut identities {
        *identity = observer
            .query_one("SELECT uuid_send(gen_random_uuid())", &[])
            .await
            .unwrap()
            .get::<_, Vec<u8>>(0)
            .try_into()
            .unwrap();
    }
    let context = EnvelopeContext {
        identities,
        plaintext_len: 1,
        header: [17; 48],
    };
    let [_, environment, library, object, generation, configuration] =
        identities.map(|id| uuid(&id));
    let scope_ids = observer
        .query_one(
            "SELECT $1::text::uuid::text, $2::text::uuid::text",
            &[&environment, &library],
        )
        .await
        .unwrap();
    let session: String = observer
        .query_one("SELECT gen_random_uuid()::text", &[])
        .await
        .unwrap()
        .get(0);
    let lease: String = observer
        .query_one("SELECT gen_random_uuid()::text", &[])
        .await
        .unwrap()
        .get(0);
    observer.batch_execute(&format!(
        "INSERT INTO public.users (id, email, created_at) VALUES ('{OWNER}', '{OWNER}@example.invalid', 1);
         INSERT INTO public.storage_environments VALUES ('{environment}');
         INSERT INTO public.storage_library_namespaces VALUES ('{library}', '{environment}', NULL);
         INSERT INTO public.storage_object_intents VALUES ('{object}', '{generation}', '{environment}',
             '{library}', '{configuration}', 1, 65, decode(repeat('11',48), 'hex'));
         INSERT INTO public.ingest_session_reservations
             (id, library_id, principal_id, idempotency_key, fingerprint, absolute_deadline,
              idle_deadline, write_lease_id, write_lease_generation)
         VALUES ('{session}', '{library}', '{OWNER}', 'lost-s3-response', decode(repeat('af',32), 'hex'),
                 clock_timestamp() + interval '24 hours', clock_timestamp() + interval '10 minutes',
                 '{lease}', '{generation}');"
    )).await.unwrap();
    let scope = CleanupScope {
        environment_id: scope_ids.get(0),
        library_id: scope_ids.get(1),
        principal_id: OWNER,
        session_id: &session,
    };
    let write_scope = CompletionScope {
        environment: identities[1],
        library: identities[2],
        principal: OWNER,
        session: &session,
        fingerprint: &[0xaf; 32],
        seal: identities[4],
        revision: 0,
        offset: 0,
    };
    let key = format!("media/{object}/{generation}");
    let created = Cell::new(false);
    let lost = session_upload::handoff(
        &mut writer,
        &write_scope,
        &lease,
        &context,
        (BUCKET, &key),
        SystemTime::now() + Duration::from_secs(60),
        async |_| {
            assert!(!created.replace(true));
            Err(()) // S3 allocated UPLOAD, but the response never arrived.
        },
    )
    .await;
    assert!(matches!(lost, Err(session_upload::HandoffError::Pending)));
    assert_eq!(saved_id(&observer, &session).await, None);

    let terminate = include_str!("../../../api/src/ingest_session_terminate.sql");
    observer.batch_execute(&format!(
        "CREATE FUNCTION pg_temp.terminate(uuid, uuid, text, uuid, text, bytea, uuid, bigint, bigint, uuid)
         RETURNS SETOF text LANGUAGE sql VOLATILE AS $sql${terminate}$sql$;"
    )).await.unwrap();
    let reason: String = observer.query_one(&format!(
        "SELECT pg_temp.terminate('{environment}', '{library}', '{OWNER}', '{session}', 'checkpoint_lost',
         decode(repeat('af',32), 'hex'), '{generation}', 0, 0, '{lease}')"
    ), &[]).await.unwrap().get(0);
    assert_eq!(reason, "aborted");

    observer
        .execute(
            "INSERT INTO public.ingest_session_cleanup_intents (session_id, kind, due_at)
         VALUES ($1::text::uuid, 'absolute_expiry', clock_timestamp() + interval '10 minutes')",
            &[&session],
        )
        .await
        .unwrap();

    let provider_scope = (identities[5], BUCKET);
    assert_eq!(
        dispatch_multipart_claims(
            &mut writer,
            scope,
            identities[2],
            provider_scope,
            (unexpected_list, unexpected_abort),
            unexpected_send,
            deadline,
        )
        .await,
        Err(DispatchError::MultipartNotConfirmed)
    );
    let sent_expiry = Cell::new(false);
    assert_eq!(
        dispatch_multipart_claims(
            &mut writer,
            scope,
            identities[1],
            provider_scope,
            (unexpected_list, unexpected_abort),
            |message, delay| {
                let CleanupMessage::UploadSessionCleanup { session_id, kind } = message;
                assert_eq!(
                    (session_id.as_str(), kind),
                    (session.as_str(), CleanupKind::AbsoluteExpiry)
                );
                assert!((1..=600).contains(&delay));
                assert!(!sent_expiry.replace(true));
                std::future::ready(Ok(()))
            },
            deadline,
        )
        .await,
        Err(DispatchError::MultipartNotConfirmed)
    );
    assert!(sent_expiry.get());
    expire_claim(&observer, &session).await;
    observer.execute(
        "UPDATE public.ingest_session_reservations SET idle_deadline = clock_timestamp() WHERE id=$1::text::uuid",
        &[&session],
    ).await.unwrap();

    let stale_id = Cell::new(None);
    let expired_during_list = async |db: &mut Client, claim: &CleanupClaim| {
        stale_id.set(Some(claim.claim_id.clone()));
        for outside in [
            CleanupScope {
                environment_id: "00000000-0000-4000-8000-000000000001",
                ..scope
            },
            CleanupScope {
                library_id: "00000000-0000-4000-8000-000000000002",
                ..scope
            },
            CleanupScope {
                principal_id: "wrong-owner",
                ..scope
            },
            CleanupScope {
                session_id: "00000000-0000-4000-8000-000000000003",
                ..scope
            },
        ] {
            assert_eq!(
                reconcile::reconcile(
                    db,
                    outside,
                    claim,
                    BUCKET,
                    &identities[5],
                    unexpected_list,
                    unexpected_abort
                )
                .await,
                Err(())
            );
        }
        for (bucket, config) in [("wrong-bucket", identities[5]), (BUCKET, identities[2])] {
            assert_eq!(
                reconcile::reconcile(
                    db,
                    scope,
                    claim,
                    bucket,
                    &config,
                    unexpected_list,
                    unexpected_abort
                )
                .await,
                Err(())
            );
        }
        reconcile::reconcile(
            db,
            scope,
            claim,
            BUCKET,
            &identities[5],
            async |observed, _| {
                assert_eq!(observed, key);
                expire_claim(&observer, &session).await;
                Ok(UPLOAD.into())
            },
            unexpected_abort,
        )
        .await
    };
    assert_eq!(
        dispatch_claims(
            &mut writer,
            scope,
            expired_during_list,
            unexpected_send,
            deadline
        )
        .await,
        Err(DispatchError::MultipartNotConfirmed)
    );
    assert_eq!(saved_id(&observer, &session).await, None);

    let stale = CleanupClaim::from_committed_row(
        session.clone(),
        CleanupKind::TerminalReconcile,
        stale_id.take().unwrap(),
        0,
    )
    .unwrap();
    let mut contender = connect().await;
    let (elapsed, timer) = tokio::sync::oneshot::channel();
    let elapsed = Cell::new(Some(elapsed));
    let timer = Cell::new(Some(timer));
    let claim_phase = Cell::new(true);
    let attempt_deadline = || {
        let terminal_timer = (!claim_phase.replace(false)).then(|| timer.take().unwrap());
        async move {
            if let Some(timer) = terminal_timer {
                timer.await.unwrap();
            } else {
                deadline().await;
            }
        }
    };
    let interrupted = dispatch_multipart_claims(
        &mut writer,
        scope,
        identities[1],
        provider_scope,
        (
            async |observed: &str, _| {
                assert_eq!(observed, key);
                assert!(created.get());
                assert_eq!(
                    reconcile::reconcile(
                        &mut contender,
                        scope,
                        &stale,
                        BUCKET,
                        &identities[5],
                        unexpected_list,
                        unexpected_abort,
                    )
                    .await,
                    Err(())
                );
                Ok(UPLOAD.into())
            },
            async |observed: &str, upload: &str, until| {
                assert_eq!((observed, upload), (key.as_str(), UPLOAD));
                assert!(SystemTime::now() < until);
                assert_eq!(saved_id(&observer, &session).await.as_deref(), Some(UPLOAD));
                // Abort reached S3, but its response is lost at the attempt deadline.
                assert!(created.replace(false));
                elapsed.take().unwrap().send(()).unwrap();
                pending().await
            },
        ),
        unexpected_send,
        attempt_deadline,
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), interrupted)
            .await
            .unwrap(),
        Err(DispatchError::MultipartNotConfirmed)
    );
    assert!(!created.get());
    expire_claim(&observer, &session).await;

    let sent = Cell::new(false);
    dispatch_multipart_claims(
        &mut writer,
        scope,
        identities[1],
        provider_scope,
        (unexpected_list, async |observed: &str, upload: &str, _| {
            assert_eq!((observed, upload), (key.as_str(), UPLOAD));
            assert!(!created.get()); // Synthetic NoSuchUpload on same ID.
            Ok(())
        }),
        |message, delay| {
            let CleanupMessage::UploadSessionCleanup { session_id, kind } = message;
            assert_eq!(
                (session_id.as_str(), kind, delay),
                (session.as_str(), CleanupKind::TerminalReconcile, 0)
            );
            assert!(!sent.replace(true));
            std::future::ready(Ok(()))
        },
        deadline,
    )
    .await
    .unwrap();
    assert!(sent.get());
    dispatch_claims(
        &mut writer,
        scope,
        async |_, _| panic!("acknowledged claim reappeared"),
        unexpected_send,
        deadline,
    )
    .await
    .unwrap();

    let state = observer
        .query_one(
            "SELECT s.checkpoint_revision, s.checkpoint_offset, s.write_lease_id IS NULL,
                c.enqueued_at IS NOT NULL, c.delivery_claim_id IS NULL
         FROM public.ingest_session_reservations AS s
         JOIN public.ingest_session_cleanup_intents AS c ON c.session_id = s.id
         WHERE s.id = $1::text::uuid AND c.kind = 'terminal_reconcile'",
            &[&session],
        )
        .await
        .unwrap();
    assert_eq!((state.get::<_, i64>(0), state.get::<_, i64>(1)), (0, 0));
    assert!((2..5).all(|index| state.get::<_, bool>(index)));
    observer
        .batch_execute(
            "DROP TABLE public.ingest_session_cleanup_intents, public.ingest_session_reservations",
        )
        .await
        .unwrap();
}
