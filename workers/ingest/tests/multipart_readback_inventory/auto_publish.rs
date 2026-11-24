use super::{Client, ReadbackFixture};
use nntune_ingest::{
    admin_auto_publish::{stage, AuthorizedRelease, CommittedFlac, ExpectedFile, StageOutcome},
    admin_review::ObjectRef,
    catalogue_publication::{PublicationError, PublishedObject},
    matching::{
        admin_publish_match::{
            release_lookup, CompleteReleaseCandidates, FileNumbering, NeedsReview,
        },
        MbSearchHit,
    },
    multipart::readback_inventory::VerifiedCompletedReadback,
};
use serde_json::json;
use tokio_postgres::Transaction;

const ALBUM: &str = "Synthetic Signals";
const RELEASE: &str = "ab320000-0000-4000-8000-000000000000";
const TRACKS: [&str; 2] = [
    "ab320000-0000-4000-8000-000000000001",
    "ab320000-0000-4000-8000-000000000002",
];
const ROWS: &str = "SELECT t.id::text, t.medium_no, t.track_no, t.mbid::text, t.title,
                           s.object_id::text, s.source_job_id
    FROM public.tracks t LEFT JOIN public.catalogue_s3_sources s ON t.id = s.track_id
    WHERE release_id = $1::text::uuid ORDER BY medium_no, track_no";

pub(super) async fn exercise(
    db: &Client,
    writer: &mut Client,
    fixture: &ReadbackFixture,
    verified: &VerifiedCompletedReadback<'_, '_>,
    receipt: PublishedObject<'_>,
    other: PublishedObject<'_>,
) {
    let object = ObjectRef {
        media: fixture.media.metadata().clone(),
        plaintext_sha256: *fixture.origin.plaintext_sha256(),
    };
    let files = ["1", "2"].map(|disc| ExpectedFile {
        numbering: FileNumbering {
            disc: Some(disc),
            track: Some("01"),
        },
        object: &object,
    });
    // Two occurrences can legitimately have identical bytes via scoped dedupe.
    let [environment, library, ..] = receipt.intent.identities;
    let job = AuthorizedRelease {
        job_id: "catalogue-old",
        owner_id: "catalogue-owner",
        environment,
        library,
        files: &files,
    };
    let committed = || CommittedFlac { verified, receipt };
    let lookup = release_lookup(ALBUM).unwrap();
    let hit = |mbid: &str| MbSearchHit {
        mbid: mbid.into(),
        title: ALBUM.into(),
        artist_name: None,
        artist_mbid: None,
        score: 0.9,
    };
    let candidates = |hits| {
        Some(CompleteReleaseCandidates {
            lookup: &lookup,
            hits,
        })
    };
    let apply =
        async |tx: &mut Transaction<'_>, media: &[CommittedFlac<'_>], rival, second_disc| {
            let mut hits = vec![hit(RELEASE)];
            if rival {
                hits.push(hit("ab330000-0000-4000-8000-000000000000"));
            }
            let rows = [1, second_disc]
                .into_iter()
                .zip(TRACKS)
                .map(|(disc, mbid)| {
                    json!({
                        "release_mbid": RELEASE, "mbid": mbid, "title": "Synthetic FLAC",
                        "medium_no": disc, "track_no": 1
                    })
                })
                .collect::<Vec<_>>();
            let body = json!({ "release_mbid": RELEASE, "recordings": rows }).to_string();
            stage(tx, &job, media, ALBUM, candidates(hits), (200, &body)).await
        };

    let both = [committed(), committed()];
    let mut tx = writer.transaction().await.unwrap();
    for (rival, second_disc, reason, code) in [
        (
            true,
            2,
            NeedsReview::ReleaseUnresolved,
            "release_unresolved",
        ),
        (false, 1, NeedsReview::PositionMismatch, "position_mismatch"),
    ] {
        assert_eq!(
            apply(&mut tx, &both, rival, second_disc).await,
            Ok(StageOutcome::NeedsReview(reason))
        );
        assert_eq!(serde_json::to_value(reason).unwrap(), code);
        assert!(tx.query(ROWS, &[&RELEASE]).await.unwrap().is_empty());
    }
    let missing_pin = PublishedObject {
        page_versions: &[],
        ..receipt
    };
    let wrong_pin = PublishedObject {
        page_versions: &["other-version"],
        ..receipt
    };
    let wrong_root = PublishedObject {
        root_version_id: [0; 16],
        ..receipt
    };
    let wrong_digest = PublishedObject {
        plaintext_sha256: &[0; 32],
        ..receipt
    };
    for invalid in [other, missing_pin, wrong_pin, wrong_root, wrong_digest] {
        let media = [
            committed(),
            CommittedFlac {
                verified,
                receipt: invalid,
            },
        ];
        assert_eq!(
            apply(&mut tx, &media, false, 2).await,
            Err(PublicationError::Refused)
        );
    }
    assert_eq!(
        apply(&mut tx, &both[..1], false, 2).await,
        Err(PublicationError::Refused)
    );
    assert!(tx.query(ROWS, &[&RELEASE]).await.unwrap().is_empty());
    tx.commit().await.unwrap();

    // A conflicting canonical second track rolls back the whole batch while
    // preserving the caller's earlier write in the outer transaction.
    let mut tx = writer.transaction().await.unwrap();
    tx.execute(
        "INSERT INTO public.tracks (release_id, medium_no, track_no, mbid, title)
         VALUES ($1::text::uuid, 2, 1, $2::text::uuid, 'Different recording')",
        &[&RELEASE, &TRACKS[0]],
    )
    .await
    .unwrap();
    assert_eq!(
        apply(&mut tx, &both, false, 2).await,
        Err(PublicationError::Refused)
    );
    let rows = tx.query(ROWS, &[&RELEASE]).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<_, i32>(1), 2);
    assert!(rows[0].get::<_, Option<&str>>(5).is_none());
    tx.rollback().await.unwrap();

    let mut tx = writer.transaction().await.unwrap();
    let StageOutcome::Staged(ids) = apply(&mut tx, &both, false, 2).await.unwrap() else {
        panic!("exact multi-disc match must stage both tracks");
    };
    assert!(db.query(ROWS, &[&RELEASE]).await.unwrap().is_empty());
    tx.commit().await.unwrap();
    let mut tx = writer.transaction().await.unwrap();
    assert_eq!(
        apply(&mut tx, &both, false, 2).await,
        Ok(StageOutcome::Staged(ids.clone()))
    );
    tx.commit().await.unwrap();
    let rows = db.query(ROWS, &[&RELEASE]).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(ids.len(), 2);
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row.get::<_, &str>(0), ids[i]);
        assert_eq!(row.get::<_, i32>(1), (i + 1) as i32);
        assert_eq!(row.get::<_, i32>(2), 1);
        assert_eq!(row.get::<_, &str>(3), TRACKS[i]);
        assert_eq!(row.get::<_, &str>(4), "Synthetic FLAC");
        assert_eq!(
            row.get::<_, String>(5).replace('-', ""),
            hex::encode(receipt.intent.identities[2])
        );
        assert_eq!(row.get::<_, &str>(6), job.job_id);
    }
}
