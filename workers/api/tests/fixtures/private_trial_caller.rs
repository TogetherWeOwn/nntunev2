use super::*;
use crate::{
    ingest_session_caller::{
        Authorization, CommittedFlacUpload, CurrentAuthorization, PreparedUpload,
    },
    private_catalogue_read::{lookup_scoped, PublicationScope},
    private_listening::PrivateListening,
    storage_digest_read::uuid_text,
    worker_entry::playback_store_for_postgres_test,
};
use nntune_core::s3_locator::S3Binding;
use nntune_ingest::{
    matching::{
        admin_publish_match::{release_lookup, CompleteReleaseCandidates, FileNumbering},
        MbSearchHit,
    },
    multipart::session_outcome::CompletionScope,
};
use private_trial::{upload, MatchEvidence, Target};
use tokio_postgres::{Client, NoTls};

#[path = "../support/blocker_observer.rs"]
mod blocker_observer;

const RECIPIENT: &str = "synthetic-trial-listener";
const OUTSIDER: &str = "synthetic-trial-outsider";
const RELEASE: &str = "ab340000-0000-4000-8000-000000000000";
const QUERY: &str = "Trial synthetic FLAC";
pub(crate) const JOB: &str = "synthetic-trial-publication";
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

pub(crate) async fn verify_registration(
    observer: &Client,
    authority: &Client,
    input: &[u8],
    auth: Authorization<'_>,
) {
    use private_trial::{
        registration::{authorize_enrollment, load_authority, Enrollment},
        TrialError,
    };
    let id = |byte| {
        let mut value = [byte; 16];
        value[6] = 0x40;
        value[8] = 0x80;
        value
    };
    let identities = [0xea, 0xeb, 0xec, 0xed, 0xee].map(id);
    let [environment, library, object, generation, _] = identities;
    let [env, lib, obj] = [environment, library, object].map(uuid_text);
    observer
        .execute(
            "INSERT INTO public.storage_environments VALUES ($1::text::uuid)",
            &[&env],
        )
        .await
        .unwrap();
    observer.execute("INSERT INTO public.storage_library_namespaces VALUES ($1::text::uuid,$2::text::uuid,NULL)", &[&lib,&env]).await.unwrap();
    observer.batch_execute(
        "GRANT SELECT (id, principal_id, library_id, fingerprint, terminal_reason) ON public.ingest_session_reservations TO upload_fixture_authority;
         GRANT SELECT (id, environment_id, owner_id) ON public.storage_library_namespaces TO upload_fixture_authority;"
    ).await.unwrap();
    observer
        .batch_execute(include_str!("private_trial_registrar_grants.sql"))
        .await
        .unwrap();
    let mut registrar = connection().await;
    registrar
        .batch_execute("SET ROLE upload_fixture_registrar")
        .await
        .unwrap();
    assert!(registrar.query_one(
        "SELECT current_user = 'upload_fixture_registrar'
            AND NOT has_any_column_privilege(current_user, 'public.native_rest_credentials', 'SELECT')
            AND NOT has_table_privilege(current_user, 'public.storage_validated_digest_winners', 'INSERT')", &[],
    ).await.unwrap().get::<_, bool>(0));
    let session = uuid_text(id(0xef));
    let fingerprint = [0x47; 32];
    let enrollment = Enrollment {
        session: &session,
        job: JOB,
        actor: &auth.record.user_id,
        fingerprint: &fingerprint,
    };
    let scope = CompletionScope {
        environment,
        library,
        principal: enrollment.actor,
        session: &session,
        fingerprint: &fingerprint,
        seal: generation,
        revision: 0,
        offset: 0,
    };
    assert!(load_authority(authority, &auth.record.id, &scope)
        .await
        .unwrap()
        .is_none());
    let authorize = async |bearer, now| {
        authorize_enrollment(
            authority,
            &auth.record.id,
            &scope,
            bearer,
            auth.peppers,
            now,
        )
        .await
    };
    assert_eq!(authorize(auth.bearer, auth.record.created_at).await, Ok(()));
    assert_eq!(
        authorize("synthetic-incorrect-credential", auth.record.created_at).await,
        Err(TrialError::Refused)
    );
    assert_eq!(
        authorize(auth.bearer, auth.record.expires_at).await,
        Err(TrialError::Refused)
    );
    let register = async |enrollment: &Enrollment<'_>, db: &mut Client, body: &[u8], ids| {
        enrollment
            .register(db, body, ids, id(0xf0), id(0xf1), &[0xac; 32])
            .await
    };
    let wrong_owner = Enrollment {
        actor: OUTSIDER,
        ..enrollment
    };
    assert!(matches!(
        register(&wrong_owner, &mut registrar, input, identities).await,
        Err(TrialError::Refused)
    ));
    let set_status = async |status: &str| {
        observer
            .execute(
                "UPDATE public.ingest_jobs SET status = $1 WHERE id = $2",
                &[&status, &JOB],
            )
            .await
            .unwrap()
    };
    assert_eq!(set_status("queued").await, 1);
    assert!(matches!(
        register(&enrollment, &mut registrar, input, identities).await,
        Err(TrialError::Refused)
    ));
    assert_eq!(set_status("done").await, 1);
    let mut owned = identities;
    owned[1] = id(0xf2);
    observer.execute(
        "INSERT INTO public.storage_library_namespaces VALUES ($1::text::uuid,$2::text::uuid,$3)",
        &[&uuid_text(owned[1]), &env, &enrollment.actor],
    ).await.unwrap();
    assert!(matches!(
        register(&enrollment, &mut registrar, input, owned).await,
        Err(TrialError::Refused)
    ));
    let mut changed = input.to_vec();
    changed[0] ^= 1;
    assert!(matches!(
        register(&enrollment, &mut registrar, &changed, identities).await,
        Err(TrialError::Refused)
    ));
    let enrolled = register(&enrollment, &mut registrar, input, identities)
        .await
        .unwrap();
    assert_eq!(enrolled.intent.reservation_generation, 1);
    assert!(matches!(
        register(&enrollment, &mut registrar, input, identities).await,
        Err(TrialError::Refused)
    ));

    let current = load_authority(authority, &auth.record.id, &scope)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.record.id, auth.record.id);
    let wrong = CompletionScope {
        fingerprint: &[0; 32],
        ..scope
    };
    assert!(load_authority(authority, &auth.record.id, &wrong)
        .await
        .unwrap()
        .is_none());
    let map = enrolled.intent.map_id;
    let [page] = enrolled
        .origin()
        .candidate_page_hashes(
            &identities,
            enrolled.intent.plaintext_len,
            &map,
            &enrolled.context.header,
        )
        .unwrap()
    else {
        panic!("expected one fixture digest page")
    };
    assert!(observer.query_one(
        "SELECT EXISTS (SELECT FROM public.ingest_session_cleanup_intents WHERE session_id=$1::text::uuid)
              AND EXISTS (SELECT FROM public.storage_object_envelope_intents WHERE object_id=$2::text::uuid AND wrapped_record=$3)
              AND EXISTS (SELECT FROM public.storage_digest_map_intents m
                JOIN public.storage_digest_map_page_intents p USING (map_id)
                WHERE m.object_id=$2::text::uuid AND m.map_id=$4::text::uuid AND m.page_count=1
                  AND p.page_index=0 AND p.expected_hash=$5)",
        &[&session, &obj, &enrolled.record.as_slice(), &uuid_text(map), &page.as_slice()],
    ).await.unwrap().get::<_, bool>(0));
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
) -> String {
    private_trial::sealed_fixture::verify_sealed_fixture(input);
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
    let empty = handle(&ctx, &library).await;
    assert_eq!((empty.status, empty.body.as_str()), (200, "[]"));

    let candidate_object = prepared.intent.identities[2];
    let candidate_id = uuid_text(candidate_object);
    observer
        .batch_execute(include_str!("private_trial_prior_media.sql"))
        .await
        .unwrap();
    let prior_track: String = observer
        .query_one(
            "SELECT pg_temp.seed_prior_trial_media($1::text::uuid, $2)",
            &[&candidate_id, &owner],
        )
        .await
        .unwrap()
        .get(0);
    observer.execute(
        "INSERT INTO public.ingest_jobs (id,user_id,idempotency_key,accepted_at,status) VALUES ($1,$2,$1,1,'done')", &[&JOB, &owner],
    ).await.unwrap();
    let scope = PublicationScope {
        environment: prepared.scope.environment,
        library: prepared.scope.library,
        provider_configuration: prepared.intent.identities[4],
        bucket: "fixture-bucket",
    };
    let published_media = async |track: &str| {
        lookup_scoped(observer, track, scope, None, None)
            .await
            .unwrap()
            .expect("committed media with an exact storage reference")
            .0
    };
    let prior_media = published_media(&prior_track).await;
    assert_ne!(prior_media.target.object, candidate_object);
    assert!(matches!(
        prior_media.locator.binding(),
        S3Binding::VersionId(version) if version == "prior-fixture-media-version"
    ));
    let prior_listing = handle(&ctx, &library).await;
    assert_eq!(prior_listing.status, 200);
    let entries: Vec<serde_json::Value> = serde_json::from_str(&prior_listing.body).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["id"], prior_track);
    let assert_only_prior_visible = async |phase| {
        let response = handle(&ctx, &library).await;
        assert_eq!(response.status, 200, "{phase}");
        assert_eq!(response.body, prior_listing.body, "{phase}");
        assert!(
            published_media(&prior_track).await == prior_media,
            "{phase}"
        );
    };

    let lookup = release_lookup(QUERY).unwrap();
    let mut publication = connection().await;
    let mut publish = async |publication: &mut Client,
                             upload_writer: &mut Client,
                             replay_bucket: Option<&str>| {
        // Keep the PostgreSQL replay/publication futures off the test stack.
        Box::pin(upload(&ctx, &request, target, async |permit, _| {
            if let Some(bucket) = replay_bucket {
                return permit
                    .replay(
                        publication,
                        upload_writer,
                        bucket,
                        auth,
                        prepared,
                        &mut read_authority,
                    )
                    .await;
            }
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
        }))
        .await
    };
    let set_job = async |job: &str| {
        observer.execute(
            "UPDATE public.ingest_session_reservations SET idempotency_key = $1 WHERE id = $2::text::uuid",
            &[&job, &prepared.scope.session],
        ).await.unwrap()
    };
    assert_eq!(set_job("other-trial-job").await, 1);
    assert_eq!(
        publish(&mut publication, upload_writer, None).await.status,
        404
    );
    assert_eq!(set_job(JOB).await, 1);
    observer.batch_execute(
        "CREATE SEQUENCE public.fixture_refused_trial_admissions;
         CREATE FUNCTION public.fixture_refuse_trial_admission() RETURNS trigger
         LANGUAGE plpgsql SET search_path = pg_catalog AS $$
         BEGIN
             IF NEW.invite_id = 931 AND EXISTS (
                 SELECT FROM public.catalogue_s3_sources s
                 JOIN public.ingest_session_reservations r
                   ON (r.idempotency_key, r.multipart_object_id) = (s.source_job_id, s.object_id)
                 WHERE (s.track_id, s.accepted_seq, s.object_id) = (NEW.track_id, NEW.accepted_seq, NEW.object_id)
                   AND r.terminal_reason = 'completed'
             ) THEN
                 -- Sequence changes survive rollback and witness the admission attempt.
                 PERFORM nextval('public.fixture_refused_trial_admissions');
                 RETURN NULL;
             END IF;
             RETURN NEW;
         END $$;
         CREATE TRIGGER fixture_refusal BEFORE INSERT ON public.private_library_admissions
         FOR EACH ROW EXECUTE FUNCTION public.fixture_refuse_trial_admission();",
    ).await.unwrap();
    let refused = publish(&mut publication, upload_writer, None).await;
    assert_eq!((refused.status, refused.body), (404, not_found().body));
    publication.simple_query("SELECT 1").await.unwrap();
    assert!(observer
        .query_one(
            "SELECT is_called AND last_value = 1 FROM public.fixture_refused_trial_admissions",
            &[],
        )
        .await
        .unwrap()
        .get::<_, bool>(0));
    assert!(observer.query_one(
        "SELECT NOT EXISTS (SELECT FROM public.catalogue_s3_sources WHERE source_job_id = $1 OR object_id = $2::text::uuid)
            AND NOT EXISTS (SELECT FROM public.storage_s3_locator_intents WHERE object_id = $2::text::uuid)
            AND NOT EXISTS (SELECT FROM public.private_library_admissions WHERE object_id = $2::text::uuid)
            AND NOT EXISTS (SELECT FROM public.storage_validated_digest_references WHERE object_id = $2::text::uuid)
            AND NOT EXISTS (SELECT FROM public.tracks WHERE release_id = $3::text::uuid)
            AND EXISTS (SELECT FROM public.ingest_session_reservations r
                JOIN public.storage_digest_inventory_intents i ON i.map_id = r.completed_map_id
                JOIN public.storage_validated_digest_winners w ON w.object_id = r.multipart_object_id
                WHERE r.id = $4::text::uuid AND r.terminal_reason = 'completed'
                  AND w.object_id = $2::text::uuid AND i.map_id = $5::text::uuid
                  AND i.media_version = $6 AND i.page_versions = $7)",
        &[&JOB, &candidate_id, &RELEASE, &prepared.scope.session,
          &committed.outcome().map, &committed.outcome().media_version, &committed.outcome().page_versions],
    ).await.unwrap().get::<_, bool>(0));
    assert_only_prior_visible("after refused admission").await;
    observer
        .batch_execute(
            "DROP TRIGGER fixture_refusal ON public.private_library_admissions;
             DROP FUNCTION public.fixture_refuse_trial_admission();
             DROP SEQUENCE public.fixture_refused_trial_admissions;",
        )
        .await
        .unwrap();

    // The registered-upload retry can reconcile only a committed publication.
    let unconfirmed = publish(&mut publication, upload_writer, Some(scope.bucket)).await;
    assert_eq!(
        (unconfirmed.status, unconfirmed.body),
        (503, unavailable().body)
    );
    assert_only_prior_visible("after the registered-upload retry").await;

    // Re-enter publication with the original authenticated completion proof.
    // Hold the real commit after the admission is staged. PostgreSQL must
    // report the publisher blocked here before the separate Library read.
    observer
        .batch_execute(
            "CREATE FUNCTION public.fixture_hold_trial_publication() RETURNS trigger
         LANGUAGE plpgsql SET search_path = pg_catalog AS $$
         BEGIN
             PERFORM pg_advisory_xact_lock(1752);
             RETURN NEW;
         END $$;
         CREATE CONSTRAINT TRIGGER fixture_hold_trial_publication
         AFTER INSERT ON public.private_library_admissions DEFERRABLE INITIALLY DEFERRED
         FOR EACH ROW EXECUTE FUNCTION public.fixture_hold_trial_publication();",
        )
        .await
        .unwrap();
    let first = {
        let mut gate = connection().await;
        let gate = gate.transaction().await.unwrap();
        let gate_pid = gate
            .query_one("SELECT pg_backend_pid(), pg_advisory_xact_lock(1752)", &[])
            .await
            .unwrap()
            .get(0);
        let publication_pid = publication
            .query_one("SELECT pg_backend_pid()", &[])
            .await
            .unwrap()
            .get(0);
        let mut pending = std::pin::pin!(publish(&mut publication, upload_writer, None));
        blocker_observer::observe_blocked(observer, gate_pid, publication_pid, pending.as_mut())
            .await
            .unwrap();
        assert_only_prior_visible("while the publication commit is blocked").await;
        gate.commit().await.unwrap();
        pending.await
    };
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
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["id"], track);
    assert_eq!(entries[0]["duration_ms"], 14_000);
    assert_eq!(entries[1]["id"], prior_track);
    assert!(published_media(&prior_track).await == prior_media);
    let published = published_media(track).await;
    assert_eq!(published.target.object, candidate_object);
    assert!(matches!(
        published.locator.binding(),
        S3Binding::VersionId(version) if version == &committed.outcome().media_version
    ));
    observer
        .batch_execute(
            "DROP TRIGGER fixture_hold_trial_publication ON public.private_library_admissions;
             DROP FUNCTION public.fixture_hold_trial_publication();",
        )
        .await
        .unwrap();
    // The database clone already contains references from the reservation race fixture.
    let exact_reference = async || {
        observer.query_one(
            "SELECT count(*) = 1 AND bool_and((environment_id, library_id, reference_id, object_id)
                   = ($1::text::uuid, $2::text::uuid, $3::text::uuid, $4::text::uuid))
             FROM public.storage_validated_digest_references
             WHERE ((environment_id, library_id) = ($1::text::uuid, $2::text::uuid) AND reference_id <> $5::text::uuid)
                OR reference_id = $3::text::uuid OR object_id = $4::text::uuid",
            &[&uuid_text(prepared.scope.environment), &uuid_text(prepared.scope.library),
              &track, &candidate_id, &prior_track],
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
    let replay = publish(&mut publication, upload_writer, Some("fixture-bucket")).await;
    assert_eq!((replay.status, replay.body), (200, first.body));
    assert_eq!(
        publish(
            &mut publication,
            upload_writer,
            Some("other-fixture-bucket")
        )
        .await
        .status,
        503
    );
    assert!(exact_reference().await);
    let set_duration = async |duration_ms: Option<i32>| {
        observer
            .execute(
                "UPDATE public.tracks SET duration_ms = $2 WHERE id = $1::text::uuid",
                &[&track, &duration_ms],
            )
            .await
            .unwrap()
    };
    for stored in [None, Some(14_001)] {
        assert_eq!(set_duration(stored).await, 1);
        assert_eq!(
            publish(&mut publication, upload_writer, None).await.status,
            404
        );
        let duration: Option<i32> = observer
            .query_one(
                "SELECT duration_ms FROM public.tracks WHERE id = $1::text::uuid",
                &[&track],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(duration, stored);
    }
    assert_eq!(set_duration(Some(14_000)).await, 1);

    observer
        .execute(
            "UPDATE public.invite_codes SET revoked_at = 3 WHERE id = 931",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        publish(&mut publication, upload_writer, Some("fixture-bucket"))
            .await
            .status,
        404
    );
    assert_eq!(handle(&ctx, &library).await.status, 403);

    track.to_owned()
}

pub(crate) async fn verify_cleanup(
    observer: &Client,
    prepared: PreparedUpload<'_>,
    track: &str,
    now: u64,
) {
    use private_trial::{
        cleanup::{cleanup, Fixture},
        TrialError,
    };
    let mut publication = connection().await;
    let publication = &mut publication;
    let scope = prepared.scope;
    let object = prepared.intent.identities[2];
    let object_id = uuid_text(object);
    let fixture = Fixture {
        environment: scope.environment,
        library: scope.library,
        object,
        track,
        session: scope.session,
        job: JOB,
        issuer: scope.principal,
        invite: 931,
        bucket: "fixture-bucket",
        cutoff: now as i64 - 1,
    };
    let retained = async |future: bool| {
        observer.execute(
            "UPDATE public.storage_validated_digest_winners
             SET retain_until = clock_timestamp() - interval '1 second',
                 read_until = CASE WHEN $2 THEN clock_timestamp() + interval '5 minutes' ELSE '-infinity' END
             WHERE object_id = $1::text::uuid",
            &[&object_id, &future],
        ).await.unwrap()
    };
    assert_eq!(retained(true).await, 1);
    let unexpected =
        async |_: &str, _: &str, _: &str| panic!("ineligible cleanup reached the provider");
    assert!(!cleanup(publication, &fixture, unexpected).await.unwrap());
    assert_eq!(retained(false).await, 1);
    let wrong = Fixture {
        invite: 932,
        ..fixture
    };
    assert!(!cleanup(publication, &wrong, unexpected).await.unwrap());
    let before_cutoff = Fixture {
        cutoff: now as i64 + 300,
        ..fixture
    };
    assert!(!cleanup(publication, &before_cutoff, unexpected)
        .await
        .unwrap());

    let other_track: String = observer
        .query_one(
            "INSERT INTO public.tracks (release_id, title, medium_no, track_no)
         VALUES ($1::text::uuid, 'Unrelated cleanup control', 1, 2) RETURNING id::text",
            &[&RELEASE],
        )
        .await
        .unwrap()
        .get(0);
    observer.execute(
        "INSERT INTO public.catalogue_s3_sources (track_id, source_job_id, accepted_seq, object_id)
         SELECT $1::text::uuid, source_job_id, accepted_seq, object_id
         FROM public.catalogue_s3_sources WHERE track_id = $2::text::uuid",
        &[&other_track, &track],
    ).await.unwrap();
    assert!(!cleanup(publication, &fixture, unexpected).await.unwrap());
    assert_eq!(
        observer
            .execute(
                "DELETE FROM public.catalogue_s3_sources WHERE track_id = $1::text::uuid",
                &[&other_track],
            )
            .await
            .unwrap(),
        1
    );
    observer
        .batch_execute(
        "INSERT INTO public.invite_codes (id, code_hash, used, created_at, expires_at, issued_by)
         SELECT 932, 'auxiliary-trial-invite', true, created_at, expires_at, issued_by
         FROM public.invite_codes WHERE id = 931;
         INSERT INTO public.private_library_admissions
         SELECT 932, recipient_id, track_id, accepted_seq, object_id
         FROM public.private_library_admissions WHERE invite_id = 931;",
        )
        .await
        .unwrap();
    assert!(!cleanup(publication, &fixture, unexpected).await.unwrap());
    assert_eq!(
        observer
            .execute("DELETE FROM public.invite_codes WHERE id = 932", &[])
            .await
            .unwrap(),
        1
    );

    let reference_count = async || {
        observer.query_one(
        "SELECT count(*) FROM public.storage_validated_digest_references WHERE object_id <> $1::text::uuid",
        &[&object_id],
    ).await.unwrap().get::<_, i64>(0)
    };
    let other_references = reference_count().await;
    let generation = uuid_text(scope.seal);
    let map = uuid_text(prepared.intent.map_id);
    let media = format!("media/{object_id}/{generation}");
    let page = format!("media-digests/{object_id}/{generation}/{map}/00000000");
    let mut versions = std::collections::BTreeSet::from([
        (media.clone(), "media-fixture-version".to_owned()),
        (media.clone(), "unrelated-fixture-version".to_owned()),
        (page.clone(), "page-fixture-version".to_owned()),
    ]);
    let interrupted = cleanup(
        publication,
        &fixture,
        async |bucket: &str, key: &str, version: &str| {
            assert_eq!(bucket, fixture.bucket);
            if key == page && version == "page-fixture-version" {
                return Err(TrialError::Unconfirmed);
            }
            assert_eq!((key, version), (media.as_str(), "media-fixture-version"));
            assert!(versions.remove(&(key.to_owned(), version.to_owned())));
            Ok(())
        },
    )
    .await;
    assert_eq!(interrupted, Err(TrialError::Unconfirmed));
    observer
        .execute("DELETE FROM public.invite_codes WHERE id = 931", &[])
        .await
        .unwrap();
    let has_source = async || {
        observer.query_one(
            "SELECT EXISTS (SELECT FROM public.catalogue_s3_sources WHERE track_id = $1::text::uuid)",
            &[&track],
        ).await.unwrap().get::<_, bool>(0)
    };
    assert!(
        has_source().await,
        "interrupted cleanup must keep the source"
    );
    for _ in 0..2 {
        let mut expected = [
            (media.as_str(), "media-fixture-version"),
            (page.as_str(), "page-fixture-version"),
        ]
        .into_iter();
        assert!(cleanup(
            publication,
            &fixture,
            async |bucket: &str, key: &str, version: &str| {
                assert_eq!(bucket, fixture.bucket);
                assert_eq!(Some((key, version)), expected.next());
                versions.remove(&(key.to_owned(), version.to_owned()));
                Ok(())
            }
        )
        .await
        .unwrap());
        assert_eq!(expected.next(), None);
        assert!(
            !has_source().await,
            "confirmed deletion must remove the source"
        );
    }
    assert_eq!(
        versions,
        std::collections::BTreeSet::from([(media, "unrelated-fixture-version".to_owned())])
    );
    assert_eq!(reference_count().await, other_references);
    assert!(observer.query_one(
        "SELECT deleting_at IS NOT NULL
            AND NOT EXISTS (SELECT FROM public.private_library_admissions a WHERE a.object_id = w.object_id)
            AND NOT EXISTS (SELECT FROM public.catalogue_s3_sources s WHERE s.object_id = w.object_id)
            AND NOT EXISTS (SELECT FROM public.storage_validated_digest_references r WHERE r.object_id = w.object_id)
         FROM public.storage_validated_digest_winners w WHERE w.object_id = $1::text::uuid",
        &[&object_id],
    ).await.unwrap().get::<_, bool>(0));
}
