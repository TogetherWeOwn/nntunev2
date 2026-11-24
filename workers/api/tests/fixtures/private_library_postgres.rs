use super::*;
use crate::{
    private_library::{page, stage_admission, Admission},
    worker_entry::playback_store_for_postgres_test,
};
use nntune_ingest::catalogue_publication::{stage, PublicationError::Refused, PublishedObject};
use tokio_postgres::{Client, NoTls};

mod publication_fixture {
    include!("private_library_publication.rs");
}

const DB: &str = "postgresql://postgres:ci-throwaway@127.0.0.1:5432/nntune_private_library_ci";
const ISSUER: &str = "catalogue-owner";
const RECIPIENT: &str = "library-invitee";
const OTHER_MEMBER: &str = "library-unrelated";
const JOB: &str = "catalogue-new";
const MBID: &str = "ab310000-0000-4000-8000-000000000000";

async fn connect() -> Client {
    assert!(
        std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
            && std::env::var("RUNNER_ENVIRONMENT").as_deref() == Ok("github-hosted")
            && std::env::var("NNTUNE_TEST_DATABASE_URL").as_deref() == Ok(DB),
        "disposable hosted Library fixture required"
    );
    let (client, connection) = tokio_postgres::connect(DB, NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    client
        .batch_execute("SET statement_timeout = '5s'; SET lock_timeout = '3s'")
        .await
        .unwrap();
    client
}

#[tokio::test]
async fn publishes_and_admits_only_the_fresh_canonical_track() {
    let mut writer = connect().await;
    let fixture = publication_fixture::PublicationFixture::load(&writer, JOB).await;
    let source = fixture.source();
    let mut h = harness();
    h.config.private_listening = Some(fixture.policy(ISSUER));
    let policy = h.config.private_listening.as_ref().unwrap();
    let store = playback_store_for_postgres_test(|| async { Ok(connect().await) });
    let ctx = Ctx {
        cfg: &h.config,
        store: &store,
        mailer: &h.mailer,
        dialer: &h.dialer,
        dns: &h.dns,
        stripe: &h.stripe,
        now: NOW,
        source_ip: h.ctx().source_ip,
    };
    let session = async |user| Request {
        method: "GET".into(),
        path: "/api/library".into(),
        cookie: Some(cookie_of(&ctx.start_session(user).await.unwrap())),
        origin: None,
        authorization: None,
        stripe_signature: None,
        body: Vec::new(),
    };
    let request = session(RECIPIENT).await;
    let unrelated = session(OTHER_MEMBER).await;
    let outsider = session("catalogue-other").await;
    assert!(ids(handle(&ctx, &request).await).is_empty());
    assert_eq!(handle(&ctx, &outsider).await.status, 403);
    let occurrence = fixture.occurrence(JOB, ISSUER, MBID);
    let mut tx = writer.transaction().await.unwrap();
    assert_eq!(
        stage(
            &mut tx,
            &occurrence,
            &PublishedObject {
                page_versions: &["mismatched-pin"],
                ..source
            }
        )
        .await,
        Err(Refused)
    );
    tx.commit().await.unwrap();
    assert!(ids(handle(&ctx, &request).await).is_empty());

    let mut tx = writer.transaction().await.unwrap();
    let track = stage(&mut tx, &occurrence, &source).await.unwrap();
    let admission = Admission {
        issuer: ISSUER,
        recipient: RECIPIENT,
        invite_id: 901,
        track_id: &track,
        job_id: JOB,
        plaintext_sha256: source.plaintext_sha256,
    };
    for (config, attempt) in [
        (None, admission),
        (
            Some(policy),
            Admission {
                issuer: OTHER_MEMBER,
                ..admission
            },
        ),
        (
            Some(policy),
            Admission {
                recipient: OTHER_MEMBER,
                ..admission
            },
        ),
        (
            Some(policy),
            Admission {
                plaintext_sha256: &[0; 32],
                ..admission
            },
        ),
    ] {
        assert!(!stage_admission(&tx, config, attempt).await.unwrap());
    }
    assert!(page(&tx, RECIPIENT, policy).await.unwrap().is_empty());
    for (role, expected) in [("member", false), ("admin", true)] {
        tx.execute(
            "UPDATE users SET role = $1 WHERE id = $2",
            &[&role, &ISSUER],
        )
        .await
        .unwrap();
        assert_eq!(
            stage_admission(&tx, Some(policy), admission).await.unwrap(),
            expected
        );
    }
    assert!(ids(handle(&ctx, &request).await).is_empty());
    tx.commit().await.unwrap();

    let mut tx = writer.transaction().await.unwrap();
    assert_eq!(stage(&mut tx, &occurrence, &source).await.unwrap(), track);
    assert!(stage_admission(&tx, Some(policy), admission).await.unwrap());
    tx.commit().await.unwrap();
    assert_eq!(ids(handle(&ctx, &request).await), [track.clone()]);
    assert_ne!(track, MBID);
    assert!(ids(handle(&ctx, &unrelated).await).is_empty());
    writer
        .execute(
            "INSERT INTO public.library (job_id, user_id, idempotency_key)
         SELECT job, $1, job FROM (
           SELECT 'legacy-' || lpad(num::text, 2, '0') AS job FROM generate_series(1, 50) num
         ) fixtures",
            &[&RECIPIENT],
        )
        .await
        .unwrap();
    let merged = ids(handle(&ctx, &request).await);
    let legacy_ids: Vec<_> = (1..=50).rev().map(|i| format!("legacy-{i:02}")).collect();
    assert_eq!(merged.len(), 50);
    assert_eq!(merged[0], track);
    assert_eq!(merged[1..], legacy_ids[..49]);

    let mut tx = writer.transaction().await.unwrap();
    let mut admitted = vec![track.clone()];
    for position in 100..150 {
        let mut next = fixture.occurrence(JOB, ISSUER, MBID);
        next.track.track_no = position;
        let id = stage(&mut tx, &next, &source).await.unwrap();
        assert!(stage_admission(
            &tx,
            Some(policy),
            Admission {
                track_id: &id,
                ..admission
            }
        )
        .await
        .unwrap());
        admitted.push(id);
    }
    tx.commit().await.unwrap();
    admitted.sort_unstable();
    writer
        .batch_execute("ALTER TABLE public.library RENAME TO legacy_fixture")
        .await
        .unwrap();
    let full = ids(handle(&ctx, &request).await);
    assert_eq!(full, admitted[..50]);
    writer
        .batch_execute("ALTER TABLE public.legacy_fixture RENAME TO library")
        .await
        .unwrap();
    writer
        .execute(
            "UPDATE public.invite_codes SET revoked_at = 3 WHERE id = 901",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(handle(&ctx, &request).await.status, 403);
    assert!(page(&writer, RECIPIENT, policy).await.unwrap().is_empty());
    writer
        .execute(
            "UPDATE public.invite_codes SET revoked_at = NULL WHERE id = 901",
            &[],
        )
        .await
        .unwrap();
    writer
        .batch_execute("DROP TABLE public.private_library_admissions")
        .await
        .unwrap();
    assert_eq!(handle(&ctx, &request).await.status, 500);
    let mut legacy = harness();
    let mut policy = fixture.policy(ISSUER);
    policy.library_scope = None;
    legacy.config.private_listening = Some(policy);
    let legacy_ctx = Ctx {
        cfg: &legacy.config,
        ..ctx
    };
    assert_eq!(ids(handle(&legacy_ctx, &request).await), legacy_ids);
}

fn ids(response: Response) -> Vec<String> {
    assert_eq!(response.status, 200);
    serde_json::from_str::<Vec<serde_json::Value>>(&response.body)
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_str().unwrap().to_owned())
        .collect()
}
