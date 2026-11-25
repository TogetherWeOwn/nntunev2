use super::{read_completion, Client};
use nntune_core::s3_locator::S3Binding;
use nntune_ingest::{
    admin_auto_publish::{
        completed_upload::{stage_completed_release, VerifiedUpload},
        AuthorizedRelease, ExpectedFile, StageOutcome,
    },
    admin_review::ObjectRef,
    catalogue_publication::PublishedObject,
    matching::{
        admin_publish_match::{release_lookup, CompleteReleaseCandidates, FileNumbering},
        MbSearchHit,
    },
};
use serde_json::json;
use std::time::{Duration, SystemTime};
use tokio_postgres::types::Type;

const TITLE: &str = "Synthetic same-title track";

pub(super) async fn exercise(
    db: &Client,
    writer: &mut Client,
    objects: &[ObjectRef; 2],
    uploads: &[VerifiedUpload<'_>; 2],
) {
    let fixtures = [
        (
            "catalogue-old",
            "Synthetic First Release",
            "ab350000-0000-4000-8000-000000000000",
            "ab350000-0000-4000-8000-000000000001",
        ),
        (
            "catalogue-new",
            "Synthetic Second Release",
            "ab360000-0000-4000-8000-000000000000",
            "ab360000-0000-4000-8000-000000000001",
        ),
    ];
    let [first, second] = uploads.each_ref().map(|upload| &upload.flac.receipt);
    assert_ne!(uploads[0].scope.session, uploads[1].scope.session);
    assert_ne!(first.locator.key(), second.locator.key());
    assert!(
        first.locator.binding() != second.locator.binding(),
        "independent uploads must have distinct media versions"
    );
    assert_ne!(first.plaintext_sha256, second.plaintext_sha256);

    let publish = async |writer: &mut Client, index: usize| {
        let (job_id, album, release, recording) = fixtures[index];
        let upload = &uploads[index];
        let files = [ExpectedFile {
            numbering: FileNumbering {
                disc: Some("1"),
                track: Some("1"),
            },
            object: &objects[index],
        }];
        let job = AuthorizedRelease {
            job_id,
            owner_id: upload.scope.principal,
            environment: upload.scope.environment,
            library: upload.scope.library,
            files: &files,
        };
        let lookup = release_lookup(album).unwrap();
        let candidates = CompleteReleaseCandidates {
            lookup: &lookup,
            hits: vec![MbSearchHit {
                mbid: release.into(),
                title: album.into(),
                artist_name: None,
                artist_mbid: None,
                score: 0.9,
            }],
        };
        let recordings = json!({
            "release_mbid": release,
            "recordings": [{
                "release_mbid": release, "mbid": recording, "title": TITLE,
                "medium_no": 1, "track_no": 1
            }]
        })
        .to_string();
        let mut tx = writer.transaction().await.unwrap();
        let result = stage_completed_release(
            &mut tx,
            &job,
            std::slice::from_ref(upload),
            album,
            Some(candidates),
            (200, &recordings),
            async |scope| read_completion(db, scope).await,
        )
        .await
        .unwrap();
        let StageOutcome::Staged(ids) = result else {
            panic!("synthetic upload must publish: {result:?}");
        };
        let [track]: [String; 1] = ids.try_into().expect("one track per upload");
        let deadline = SystemTime::now() + Duration::from_secs(60);
        let attached: bool = tx
            .query_one(
                include_str!("../../../api/tests/fixtures/private_catalogue_attach.sql"),
                &[&track, &deadline],
            )
            .await
            .unwrap()
            .get(0);
        assert!(
            attached,
            "the published track must retain its digest winner"
        );
        tx.commit().await.unwrap();
        track
    };

    let first_track = publish(writer, 0).await;
    assert_published(db, &first_track, first, fixtures[0].2).await;

    let second_track = publish(writer, 1).await;
    assert_ne!(first_track, second_track);
    assert_published(db, &first_track, first, fixtures[0].2).await;
    assert_published(db, &second_track, second, fixtures[1].2).await;
}

async fn assert_published(db: &Client, track: &str, receipt: &PublishedObject<'_>, release: &str) {
    let row = db
        .query_one(
            "SELECT title, release_id::text FROM public.tracks WHERE id = $1::text::uuid",
            &[&track],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, &str>(0), TITLE);
    assert_eq!(row.get::<_, &str>(1), release);

    let [environment, library, _, _, configuration] = receipt.intent.identities.map(hex::encode);
    let S3Binding::VersionId(version) = receipt.locator.binding() else {
        panic!("fixture must have an immutable version");
    };
    let row = db
        .query_typed_one(
            include_str!("../../../api/src/private_catalogue_read.sql"),
            &[
                (&track, Type::TEXT),
                (&environment, Type::TEXT),
                (&library, Type::TEXT),
                (&None::<i64>, Type::INT8),
                (&configuration, Type::TEXT),
                (&receipt.locator.bucket(), Type::TEXT),
                (version, Type::TEXT),
                (&None::<&str>, Type::TEXT),
            ],
        )
        .await
        .expect("track must resolve at its published version");
    assert_eq!(row.get::<_, &str>("s3_version_id"), version);
    assert_eq!(row.get::<_, &str>("s3_key"), receipt.locator.key());
    assert_eq!(
        row.get::<_, &[u8]>("plaintext_sha256"),
        receipt.plaintext_sha256
    );
}
