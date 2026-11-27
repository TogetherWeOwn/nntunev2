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
mod publication_locks {
    include!("private_trial_publication_locks.rs");
}

mod playlist_membership {
    include!("private_library_playlist_membership.rs");
}

const DB: &str = "postgresql://postgres:ci-throwaway@127.0.0.1:5432/nntune_private_library_ci";
const ISSUER: &str = "catalogue-owner";
const RECIPIENT: &str = "library-invitee";
const OTHER_MEMBER: &str = "library-unrelated";
const JOB: &str = "catalogue-new";
const MBID: &str = "ab310000-0000-4000-8000-000000000000";

async fn connect() -> Client {
    connect_fixture(DB).await
}

async fn connect_fixture(db: &str) -> Client {
    assert!(
        std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
            && std::env::var("RUNNER_ENVIRONMENT").as_deref() == Ok("github-hosted")
            && std::env::var("NNTUNE_TEST_DATABASE_URL").as_deref() == Ok(db),
        "disposable hosted Library fixture required"
    );
    let (client, connection) = tokio_postgres::connect(db, NoTls).await.unwrap();
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
    let libraries = playlist_membership::create_empty_libraries(&writer).await;
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
    playlist_membership::assert_no_memberships(&writer).await;

    let mut tx = writer.transaction().await.unwrap();
    assert_eq!(stage(&mut tx, &occurrence, &source).await.unwrap(), track);
    assert!(stage_admission(&tx, Some(policy), admission).await.unwrap());
    tx.commit().await.unwrap();
    playlist_membership::assert_no_memberships(&writer).await;
    assert_eq!(ids(handle(&ctx, &request).await), [track.clone()]);
    assert_ne!(track, MBID);
    assert!(ids(handle(&ctx, &unrelated).await).is_empty());
    let playlist =
        playlist_membership::exercise(&mut writer, &ctx, &request, &unrelated, &track, libraries)
            .await;
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
    verify_replacement(&mut writer, &ctx, &unrelated, &fixture, &track).await;
    writer
        .execute(
            "UPDATE public.invite_codes SET revoked_at = 3 WHERE id = 901",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(handle(&ctx, &request).await.status, 403);
    assert!(page(&writer, RECIPIENT, policy).await.unwrap().is_empty());
    playlist_membership::assert_denied(
        private_playlists::route(
            &ctx,
            &Request {
                origin: Some(ORIGIN.into()),
                ..request.clone()
            },
            &mut writer,
            private_playlists::Command::Append(&playlist, track.clone()),
        )
        .await,
    );
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

async fn verify_replacement(
    writer: &mut Client,
    ctx: &Ctx<'_, impl Store + SingleUseStore, impl Mailer, impl Dialer, impl AsyncDns>,
    request: &Request,
    original: &publication_fixture::PublicationFixture,
    track: &str,
) {
    const COMPANION_JOB: &str = "catalogue-library-companion";
    const REPLACEMENT_JOB: &str = "catalogue-library-replacement";
    writer
        .execute(
            "INSERT INTO public.ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
             VALUES ($1, $3, $1, 1, 'done'), ($2, $3, $2, 1, 'done')",
            &[&COMPANION_JOB, &REPLACEMENT_JOB, &ISSUER],
        )
        .await
        .unwrap();
    let policy = ctx.cfg.private_listening.as_ref();
    let admission = Admission {
        issuer: ISSUER,
        recipient: OTHER_MEMBER,
        invite_id: 902,
        track_id: track,
        job_id: JOB,
        plaintext_sha256: original.source().plaintext_sha256,
    };
    let mut companion = original.occurrence(COMPANION_JOB, ISSUER, MBID);
    companion.track.track_no = 150;
    companion.track.title = "Synthetic companion";
    let mut tx = writer.transaction().await.unwrap();
    // Give the pre-existing canonical row a duration to preserve across replacement.
    const STORED_DURATION_MS: i32 = 14_000;
    assert_eq!(
        tx.execute(
            "UPDATE public.tracks SET duration_ms = $2 WHERE id = $1::text::uuid",
            &[&track, &STORED_DURATION_MS],
        )
        .await
        .unwrap(),
        1
    );
    let companion_id = stage(&mut tx, &companion, &original.source())
        .await
        .unwrap();
    assert!(stage_admission(&tx, policy, admission).await.unwrap());
    assert!(stage_admission(
        &tx,
        policy,
        Admission {
            track_id: &companion_id,
            job_id: COMPANION_JOB,
            ..admission
        },
    )
    .await
    .unwrap());
    tx.commit().await.unwrap();

    let listing = async || entries(handle(ctx, request).await);
    let [companion_row, original_row]: [serde_json::Value; 2] = listing().await.try_into().unwrap();
    assert_eq!(companion_row["id"], companion_id);
    assert_eq!(original_row["id"], track);
    assert_eq!(original_row["title"], "Fresh synthetic track");
    assert_eq!(original_row["duration_ms"], STORED_DURATION_MS);

    let replacement = publication_fixture::PublicationFixture::load_replacement(writer).await;
    let source = replacement.source();
    let mut next = replacement.occurrence(REPLACEMENT_JOB, ISSUER, MBID);
    next.track.title = "Replacement match title";
    let mut tx = writer.transaction().await.unwrap();
    assert_eq!(stage(&mut tx, &next, &source).await.unwrap(), track);
    assert!(stage_admission(
        &tx,
        policy,
        Admission {
            job_id: REPLACEMENT_JOB,
            plaintext_sha256: source.plaintext_sha256,
            ..admission
        },
    )
    .await
    .unwrap());
    assert_eq!(
        listing().await,
        [companion_row.clone(), original_row.clone()]
    );
    tx.commit().await.unwrap();

    // Library order reflects the new admission sequence. The canonical row is stable.
    assert_eq!(listing().await, [original_row, companion_row]);
}

fn ids(response: Response) -> Vec<String> {
    entries(response)
        .iter()
        .map(|entry| entry["id"].as_str().unwrap().to_owned())
        .collect()
}

fn entries(response: Response) -> Vec<serde_json::Value> {
    assert_eq!(response.status, 200);
    serde_json::from_str(&response.body).unwrap()
}
