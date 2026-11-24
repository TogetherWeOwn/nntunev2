use super::Client;
use nntune_ingest::{
    admin_auto_publish::{
        completed_upload::{stage_completed_release, VerifiedUpload},
        stage, AuthorizedRelease, CommittedFlac, ExpectedFile, StageOutcome,
    },
    admin_review::ObjectRef,
    catalogue_publication::{PublicationError, PublishedObject},
    matching::{
        admin_publish_match::{
            release_lookup, CompleteReleaseCandidates, FileNumbering, NeedsReview,
        },
        MbSearchHit,
    },
    multipart::session_outcome::{CommittedOutcome, CompletionScope},
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
const READ: &str = include_str!("../fixtures/catalogue_completed_upload_read.sql");

fn scope_key(scope: &CompletionScope<'_>) -> [String; 8] {
    [
        hex::encode(scope.environment),
        hex::encode(scope.library),
        scope.principal.into(),
        scope.session.into(),
        hex::encode(scope.fingerprint),
        hex::encode(scope.seal),
        scope.revision.to_string(),
        scope.offset.to_string(),
    ]
}

#[derive(Clone, Copy)]
enum ReleaseFixtureEvidence<'a> {
    Receipt(&'a [CommittedFlac<'a>]),
    Scoped(&'a [VerifiedUpload<'a>]),
    Indeterminate(&'a [VerifiedUpload<'a>]),
    Conflicting(&'a [VerifiedUpload<'a>], usize),
}

pub(super) async fn exercise(
    db: &Client,
    writer: &mut Client,
    outcome_writer: &mut Client,
    objects: &[ObjectRef; 2],
    both: [CommittedFlac<'_>; 2],
) {
    use ReleaseFixtureEvidence::{Conflicting, Indeterminate, Receipt, Scoped};
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
        async |tx: &mut Transaction<'_>, media: ReleaseFixtureEvidence<'_>, rival, second_disc| {
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
            let uploads = match media {
                Receipt(receipts) => {
                    return stage(tx, &job, receipts, ALBUM, Some(candidates), (200, &body)).await;
                }
                Scoped(uploads) | Indeterminate(uploads) | Conflicting(uploads, _) => uploads,
            };
            stage_completed_release(
                tx,
                &job,
                uploads,
                ALBUM,
                Some(candidates),
                (200, &body),
                async |scope| {
                    let row = db.query_opt(READ, &[&&scope_key(scope)[..]]).await?;
                    if matches!(media, Indeterminate(_)) {
                        return Err(PublicationError::Indeterminate);
                    }
                    Ok(row.map(|row| {
                        let mut outcome = CommittedOutcome {
                            map: row.get(0),
                            object: row.get(1),
                            generation: row.get(2),
                            configuration: row.get(3),
                            root_version: row.get(4),
                            media_version: row.get(5),
                            page_versions: row.get(6),
                        };
                        if let Conflicting(_, pin) = media {
                            [
                                &mut outcome.map,
                                &mut outcome.object,
                                &mut outcome.generation,
                                &mut outcome.configuration,
                                &mut outcome.root_version,
                                &mut outcome.media_version,
                                &mut outcome.page_versions[0],
                            ][pin]
                                .push('!');
                        }
                        outcome
                    }))
                },
            )
            .await
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
            apply(&mut tx, Receipt(&both), rival, second_disc).await,
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
            apply(&mut tx, Receipt(media), false, 2).await,
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
        apply(&mut tx, Receipt(&both), false, 2).await,
        Err(PublicationError::Refused)
    );
    assert!(tx.query(ROWS, &[&RELEASE]).await.unwrap().is_empty());
    tx.rollback().await.unwrap();

    let sessions = both
        .each_ref()
        .map(|flac| hex::encode(flac.receipt.intent.identities[2]));
    let uploads = std::array::from_fn::<_, 2, _>(|i| VerifiedUpload {
        scope: CompletionScope {
            environment,
            library,
            principal: job.owner_id,
            session: &sessions[i],
            fingerprint: &[0x83; 32],
            seal: both[i].receipt.intent.identities[3],
            revision: 1,
            offset: both[i].receipt.intent.plaintext_len as i64,
        },
        flac: both[i],
    });
    let mut tx = writer.transaction().await.unwrap();
    for commit in [false, true] {
        let result = outcome_writer.transaction().await.unwrap();
        for upload in &uploads {
            result
                .execute(
                    "INSERT INTO public.test_committed_uploads VALUES ($1, $2::text::uuid)",
                    &[
                        &&scope_key(&upload.scope)[..],
                        &hex::encode(upload.flac.receipt.intent.map_id),
                    ],
                )
                .await
                .unwrap();
        }
        assert_eq!(
            apply(&mut tx, Scoped(&uploads), false, 2).await,
            Err(PublicationError::Refused)
        );
        if commit {
            result.commit().await.unwrap();
        } else {
            result.rollback().await.unwrap();
            assert_eq!(
                apply(&mut tx, Scoped(&uploads), false, 2).await,
                Err(PublicationError::Refused)
            );
        }
    }
    let mut foreign = uploads;
    foreign[1].scope.principal = "catalogue-other";
    let mut wrong_request = uploads;
    wrong_request[1].scope.fingerprint = &[0; 32];
    let mut mismatched = uploads;
    mismatched[1].flac.verified = uploads[0].flac.verified;
    // A non-final scope must refuse before the indeterminate reader runs.
    let mut wrong_seal = uploads;
    wrong_seal[0].scope.seal = [0; 16];
    let mut wrong_offset = uploads;
    wrong_offset[0].scope.offset = 0;
    for evidence in [
        Scoped(&mismatched),
        Scoped(&foreign),
        Scoped(&wrong_request),
        Indeterminate(&wrong_seal),
        Indeterminate(&wrong_offset),
    ]
    .into_iter()
    .chain((0..7).map(|pin| Conflicting(&uploads, pin)))
    {
        assert_eq!(
            apply(&mut tx, evidence, false, 2).await,
            Err(PublicationError::Refused)
        );
        assert!(tx.query(ROWS, &[&RELEASE]).await.unwrap().is_empty());
    }
    assert_eq!(
        apply(&mut tx, Indeterminate(&uploads), false, 2).await,
        Err(PublicationError::Indeterminate)
    );
    assert!(tx.query(ROWS, &[&RELEASE]).await.unwrap().is_empty());

    // Simulate a lost acknowledgement on either side of COMMIT. One retry
    // stages after rollback, and one replays the already committed rows.
    assert!(matches!(
        apply(&mut tx, Scoped(&uploads), false, 2).await,
        Ok(StageOutcome::Staged(_))
    ));
    tx.rollback().await.unwrap();
    let mut tx = writer.transaction().await.unwrap();
    let StageOutcome::Staged(ids) = apply(&mut tx, Scoped(&uploads), false, 2).await.unwrap()
    else {
        panic!("exact multi-disc match must stage both tracks");
    };
    assert!(db.query(ROWS, &[&RELEASE]).await.unwrap().is_empty());
    tx.commit().await.unwrap();
    let mut tx = writer.transaction().await.unwrap();
    assert_eq!(
        apply(&mut tx, Scoped(&uploads), false, 2).await,
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
