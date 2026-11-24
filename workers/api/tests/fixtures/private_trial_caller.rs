use super::*;
use crate::{
    ingest_session_caller::{
        Authorization, CommittedFlacUpload, CurrentAuthorization, PreparedUpload,
    },
    private_listening::PrivateListening,
    storage_digest_read::uuid_text,
    worker_entry::playback_store_for_postgres_test,
};
use nntune_ingest::{
    matching::{
        admin_publish_match::{release_lookup, CompleteReleaseCandidates, FileNumbering},
        MbSearchHit,
    },
    multipart::session_outcome::CompletionScope,
};
use private_trial::{upload, MatchEvidence, Target};
use tokio_postgres::{Client, NoTls};

const RECIPIENT: &str = "synthetic-trial-listener";
const OUTSIDER: &str = "synthetic-trial-outsider";
const RELEASE: &str = "ab340000-0000-4000-8000-000000000000";
const QUERY: &str = "Trial synthetic FLAC";
const JOB: &str = "synthetic-trial-publication";
const ORIGIN: &str = "https://staging.nntune.com";
const DB: &str = "postgresql://postgres:ci-throwaway@127.0.0.1:5432/nntune_upload_caller_ci";

async fn connection() -> Client {
    assert_eq!(std::env::var("GITHUB_ACTIONS").as_deref(), Ok("true"));
    assert_eq!(
        std::env::var("RUNNER_ENVIRONMENT").as_deref(),
        Ok("github-hosted")
    );
    assert_eq!(std::env::var("NNTUNE_TEST_DATABASE_URL").as_deref(), Ok(DB));
    let (client, connection) = tokio_postgres::connect(DB, NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    client
}

pub(crate) async fn verify_publication(
    observer: &Client,
    upload_writer: &mut Client,
    now: u64,
    input: &[u8],
    auth: Authorization<'_>,
    prepared: PreparedUpload<'_>,
    committed: &CommittedFlacUpload<'_, '_>,
    mut read_authority: impl AsyncFnMut(
        &str,
        &CompletionScope<'_>,
    ) -> Result<Option<CurrentAuthorization>, StoreError>,
) {
    observer.batch_execute(concat!(
        include_str!("storage_s3_locator_schema.sql"),
        include_str!("../../../ingest/tests/fixtures/catalogue_source_schema.sql"),
        include_str!("../../src/private_library_schema.sql"),
        "INSERT INTO public.users (id,email,created_at,invite_code_hash,role) VALUES
          ('synthetic-trial-listener','trial-listener@example.test',1,'synthetic-trial-code','member'),
          ('synthetic-trial-outsider','trial-outsider@example.test',1,NULL,'admin');"
    )).await.unwrap();
    let owner = prepared.scope.principal;
    observer
        .execute(
            "UPDATE public.users SET role = 'admin' WHERE id = $1",
            &[&owner],
        )
        .await
        .unwrap();
    observer.execute(
        "INSERT INTO public.ingest_jobs (id,user_id,idempotency_key,accepted_at,status) VALUES ($1,$2,$1,1,'done')", &[&JOB, &owner],
    ).await.unwrap();
    observer.execute(
        "INSERT INTO public.invite_codes (id,code_hash,used,created_at,issued_by,email,expires_at,redeemed_by,redeemed_at)
         VALUES (931,'synthetic-trial-code',true,1,$1,'trial-listener@example.test',10,$2,2)", &[&owner,&RECIPIENT],
    ).await.unwrap();

    let dashed = |id| {
        let id = uuid_text(id);
        format!(
            "{}-{}-{}-{}-{}",
            &id[..8],
            &id[8..12],
            &id[12..16],
            &id[16..20],
            &id[20..]
        )
    };
    let mut h = harness();
    h.config.origin = ORIGIN.into();
    h.config.private_listening = PrivateListening::parse(&serde_json::json!({
        "owner_user_id": owner, "invite_ids": [931], "expires_at": now + 300,
        "library_scope": {"environment_id": dashed(prepared.scope.environment), "library_id": dashed(prepared.scope.library)}
    }).to_string());
    let store = playback_store_for_postgres_test(|| async { Ok(connection().await) });
    let ctx = Ctx {
        cfg: &h.config,
        store: &store,
        mailer: &h.mailer,
        dialer: &h.dialer,
        dns: &h.dns,
        stripe: &h.stripe,
        now,
        source_ip: h.ctx().source_ip,
    };
    let session = async |user| cookie_of(&ctx.start_session(user).await.unwrap());
    let mut request = Request {
        method: "POST".into(),
        path: private_trial::PATH.into(),
        cookie: Some(session(owner).await),
        origin: Some(ORIGIN.into()),
        authorization: None,
        stripe_signature: None,
        body: input.to_vec(),
    };
    let library = Request {
        method: "GET".into(),
        path: "/api/library".into(),
        cookie: Some(session(RECIPIENT).await),
        body: Vec::new(),
        ..request.clone()
    };
    let outsider = Request {
        cookie: Some(session(OUTSIDER).await),
        ..request.clone()
    };
    let target = Some(Target {
        invite_id: 931,
        job_id: JOB,
    });
    let deny = async |req: &Request, target| {
        upload(&ctx, req, target, async |_, _| {
            panic!("refusal reached upload")
        })
        .await
        .status
    };
    assert_eq!(deny(&request, None).await, 404);
    assert_eq!(deny(&outsider, target).await, 404);
    assert_eq!(
        deny(
            &Request {
                origin: None,
                ..request.clone()
            },
            target
        )
        .await,
        404
    );
    request.body[0] ^= 1;
    assert_eq!(deny(&request, target).await, 400);
    request.body[0] ^= 1;
    assert_eq!(handle(&ctx, &library).await.body, "[]");

    let lookup = release_lookup(QUERY).unwrap();
    let mut publication = connection().await;
    let mut publish = async |publication: &mut Client, upload_writer: &mut Client| {
        upload(&ctx, &request, target, async |permit, _| {
            permit.preflight(publication).await?;
            let evidence = MatchEvidence {
                query: QUERY,
                candidates: Some(CompleteReleaseCandidates {
                    lookup: &lookup,
                    hits: vec![MbSearchHit {
                        mbid: RELEASE.into(),
                        title: QUERY.into(),
                        artist_name: None,
                        artist_mbid: None,
                        score: 0.9,
                    }],
                }),
                recordings: (
                    200,
                    include_str!("../../../ingest/tests/fixtures/review_retry_recordings.json"),
                ),
                numbering: FileNumbering {
                    disc: Some("1"),
                    track: Some("1"),
                },
            };
            permit
                .publish(
                    publication,
                    upload_writer,
                    auth,
                    prepared,
                    committed,
                    evidence,
                    &mut read_authority,
                )
                .await
        })
        .await
    };
    observer.batch_execute(
        "CREATE FUNCTION public.fixture_refuse_trial_admission() RETURNS trigger LANGUAGE plpgsql
         AS $$ BEGIN RETURN NULL; END $$;
         CREATE TRIGGER fixture_refusal BEFORE INSERT ON public.private_library_admissions
         FOR EACH ROW EXECUTE FUNCTION public.fixture_refuse_trial_admission();",
    ).await.unwrap();
    assert_eq!(publish(&mut publication, upload_writer).await.status, 404);
    publication.simple_query("SELECT 1").await.unwrap();
    assert!(observer
        .query_one(
            "SELECT NOT EXISTS (SELECT FROM public.catalogue_s3_sources)
              AND NOT EXISTS (SELECT FROM public.storage_s3_locator_intents)
              AND NOT EXISTS (SELECT FROM public.tracks WHERE release_id = $1::text::uuid)",
            &[&RELEASE]
        )
        .await
        .unwrap()
        .get::<_, bool>(0));
    observer
        .batch_execute("DROP TRIGGER fixture_refusal ON public.private_library_admissions")
        .await
        .unwrap();
    let first = publish(&mut publication, upload_writer).await;
    assert_eq!(first.status, 200);
    let body: serde_json::Value = serde_json::from_str(&first.body).unwrap();
    let track = body["trackId"].as_str().unwrap();
    assert_eq!(
        body["sha256"],
        "c935d3368a1f2f22ca87b8ccfa6b9facae070e6e07be1e16abff513835a5f259"
    );
    let listing = handle(&ctx, &library).await;
    assert_eq!(listing.status, 200);
    let entries: Vec<serde_json::Value> = serde_json::from_str(&listing.body).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["id"], track);
    let exact_reference = async || {
        observer.query_one(
            "SELECT count(*) = 1 AND bool_and((environment_id, library_id, reference_id, object_id)
                   = ($1::text::uuid, $2::text::uuid, $3::text::uuid, $4::text::uuid))
             FROM public.storage_validated_digest_references",
            &[&uuid_text(prepared.scope.environment), &uuid_text(prepared.scope.library),
              &track, &uuid_text(prepared.intent.identities[2])],
        ).await.unwrap().get::<_, bool>(0)
    };
    assert!(exact_reference().await);
    assert_eq!(
        handle(
            &ctx,
            &Request {
                cookie: outsider.cookie.clone(),
                ..library.clone()
            }
        )
        .await
        .status,
        403
    );
    let replay = publish(&mut publication, upload_writer).await;
    assert_eq!((replay.status, replay.body), (200, first.body));
    assert!(exact_reference().await);

    observer
        .execute(
            "UPDATE public.invite_codes SET revoked_at = 3 WHERE id = 931",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(publish(&mut publication, upload_writer).await.status, 404);
    assert_eq!(handle(&ctx, &library).await.status, 403);
}
