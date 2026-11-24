use super::Client;
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
};
use serde_json::json;
use tokio_postgres::Transaction;

const ALBUM: &str = "Synthetic Signals";
const RELEASE: &str = "ab320000-0000-4000-8000-000000000000";
const TRACKS: [&str; 2] = [
    "ab320000-0000-4000-8000-000000000001",
    "ab320000-0000-4000-8000-000000000002",
];
const ROWS: &str = "SELECT t.id::text, t.medium_no, t.track_no, t.mbid::text, s.object_id::text
    FROM public.tracks t LEFT JOIN public.catalogue_s3_sources s ON t.id = s.track_id
    WHERE release_id = $1::text::uuid ORDER BY medium_no, track_no";

pub(super) async fn exercise(
    db: &Client,
    writer: &mut Client,
    objects: &[ObjectRef; 2],
    both: [CommittedFlac<'_>; 2],
) {
    let files = [("1", &objects[0]), ("2", &objects[1])].map(|(disc, object)| ExpectedFile {
        numbering: FileNumbering {
            disc: Some(disc),
            track: Some("01"),
        },
        object,
    });
    let [environment, library, ..] = both[0].receipt.intent.identities;
    let job = AuthorizedRelease {
        job_id: "catalogue-old",
        owner_id: "catalogue-owner",
        environment,
        library,
        files: &files,
    };
    let lookup = release_lookup(ALBUM).unwrap();
    let hit = |mbid: &str| MbSearchHit {
        mbid: mbid.into(),
        title: ALBUM.into(),
        artist_name: None,
        artist_mbid: None,
        score: 0.9,
    };
    let apply =
        async |tx: &mut Transaction<'_>, media: &[CommittedFlac<'_>], rival, second_disc| {
            let mut hits = vec![hit(RELEASE)];
            if rival {
                hits.push(hit("ab330000-0000-4000-8000-000000000000"));
            }
            let candidates = CompleteReleaseCandidates {
                lookup: &lookup,
                hits,
            };
            let rows = [(1, TRACKS[0]), (second_disc, TRACKS[1])].map(|(disc, mbid)| {
                json!({
                    "release_mbid": RELEASE, "mbid": mbid, "title": "Synthetic FLAC",
                    "medium_no": disc, "track_no": 1
                })
            });
            let body = json!({ "release_mbid": RELEASE, "recordings": rows }).to_string();
            stage(tx, &job, media, ALBUM, Some(candidates), (200, &body)).await
        };

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
    let [first, second] = &both;
    let swapped = [CommittedFlac { ..*second }, CommittedFlac { ..*first }];
    let missing_pin = PublishedObject {
        page_versions: &[],
        ..second.receipt
    };
    let incomplete = [
        CommittedFlac { ..*first },
        CommittedFlac {
            receipt: missing_pin,
            ..*second
        },
    ];
    for media in [&both[..1], &swapped, &incomplete] {
        assert_eq!(
            apply(&mut tx, media, false, 2).await,
            Err(PublicationError::Refused)
        );
    }
    assert!(tx.query(ROWS, &[&RELEASE]).await.unwrap().is_empty());
    tx.commit().await.unwrap();

    // A late storage fence must roll back the first occurrence too.
    let mut tx = writer.transaction().await.unwrap();
    let second_id = hex::encode(second.receipt.intent.identities[2]);
    let quarantined = "UPDATE public.storage_validated_digest_winners
        SET quarantined = true WHERE object_id = $1::text::uuid";
    assert_eq!(tx.execute(quarantined, &[&second_id]).await.unwrap(), 1);
    assert_eq!(
        apply(&mut tx, &both, false, 2).await,
        Err(PublicationError::Refused)
    );
    assert!(tx.query(ROWS, &[&RELEASE]).await.unwrap().is_empty());
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
        assert_eq!(
            row.get::<_, String>(4).replace('-', ""),
            hex::encode(both[i].receipt.intent.identities[2])
        );
    }
}
