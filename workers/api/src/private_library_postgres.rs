use super::*;
use crate::storage_digest_read::ReadUuid;
use nntune_core::s3_locator::{S3Binding, S3Locator};
use nntune_ingest::{
    catalogue_publication::{
        stage, PublicationError::Refused, PublishedObject, VerifiedOccurrence,
    },
    digest_candidate_store::CandidateIntent,
    track_writers::TrackRow,
};
use tokio_postgres::{Client, NoTls};

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
    let reader = connect().await;
    writer
        .batch_execute(concat!(
            include_str!("private_library_schema.sql"),
            include_str!("../tests/fixtures/private_library_users.sql")
        ))
        .await
        .unwrap();
    let object = writer
        .query_one(
            include_str!("../tests/fixtures/private_library_object.sql"),
            &[&JOB],
        )
        .await
        .unwrap();
    let id = |column| object.get::<_, ReadUuid>(column).0;
    let intent = CandidateIntent {
        identities: [
            "environment_id",
            "library_id",
            "object_id",
            "generation_id",
            "provider_configuration_version_id",
        ]
        .map(id),
        plaintext_len: object.get::<_, i64>("plaintext_bytes").try_into().unwrap(),
        map_id: id("map_id"),
        header: object.get::<_, &[u8]>("header_tag").try_into().unwrap(),
        reservation_generation: 1,
    };
    let locator = S3Locator::new(
        object.get("s3_bucket"),
        object.get("s3_key"),
        S3Binding::VersionId(object.get("s3_version_id")),
    )
    .unwrap();
    let versions: Vec<&str> = object.get("page_versions");
    let source = PublishedObject {
        intent: &intent,
        locator: &locator,
        plaintext_sha256: object
            .get::<_, &[u8]>("plaintext_sha256")
            .try_into()
            .unwrap(),
        root_version_id: id("root_version_id"),
        page_versions: &versions,
    };
    let config = serde_json::json!({
        "owner_user_id": ISSUER, "invite_ids": [901, 902], "expires_at": 2_000_000_000u64,
        "library_scope": {"environment_id": object.get::<_, &str>("environment"),
                          "library_id": object.get::<_, &str>("library")}
    });
    let policy = PrivateListening::parse(&config.to_string()).unwrap();
    let visible = async |user| page(&reader, user, &policy).await.unwrap();
    let occurrence = VerifiedOccurrence {
        job_id: JOB,
        owner_id: ISSUER,
        object_id: id("object_id"),
        track: TrackRow {
            release_id: MBID,
            mbid: MBID,
            title: "Fresh synthetic track",
            medium_no: 1,
            track_no: 99,
            file_ordinal: 0,
        },
    };
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
    assert!(visible(RECIPIENT).await.is_empty());

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
            Some(&policy),
            Admission {
                issuer: OTHER_MEMBER,
                ..admission
            },
        ),
        (
            Some(&policy),
            Admission {
                recipient: OTHER_MEMBER,
                ..admission
            },
        ),
        (
            Some(&policy),
            Admission {
                plaintext_sha256: &[0; 32],
                ..admission
            },
        ),
    ] {
        assert!(!stage_admission(&tx, config, attempt).await.unwrap());
    }
    assert!(page(&tx, RECIPIENT, &policy).await.unwrap().is_empty());
    assert!(stage_admission(&tx, Some(&policy), admission)
        .await
        .unwrap());
    assert!(visible(RECIPIENT).await.is_empty());
    tx.commit().await.unwrap();

    let mut tx = writer.transaction().await.unwrap();
    assert_eq!(stage(&mut tx, &occurrence, &source).await.unwrap(), track);
    assert!(stage_admission(&tx, Some(&policy), admission)
        .await
        .unwrap());
    tx.commit().await.unwrap();
    let [entry]: [LibraryEntry; 1] = visible(RECIPIENT).await.try_into().unwrap();
    assert_eq!(entry.id, track);
    assert_ne!(entry.id, MBID);
    assert!(visible(OTHER_MEMBER).await.is_empty());
    assert!(visible("catalogue-other").await.is_empty());
    writer
        .execute(
            "UPDATE public.invite_codes SET revoked_at = 3 WHERE id = 901",
            &[],
        )
        .await
        .unwrap();
    assert!(visible(RECIPIENT).await.is_empty());
}
