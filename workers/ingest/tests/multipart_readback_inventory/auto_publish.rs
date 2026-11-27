use super::Client;
use nntune_core::s3_locator::S3Binding;
use nntune_ingest::{
    admin_auto_publish::{
        completed_upload::{stage_completed_release, VerifiedUpload},
        stage,
        telemetry::matcher_counter,
        AuthorizedRelease, CommittedFlac, ExpectedFile, StageOutcome,
    },
    admin_review::{
        completed::{retry_completed, ResolvedCompletion, ReviewTarget},
        Authorization, BoundaryError, JobView, ObjectRef, ReviewJob, ReviewStatus, READ_SCOPE,
        RETRY_SCOPE,
    },
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
use tokio_postgres::{error::SqlState, Transaction};

#[path = "independent_publications.rs"]
mod independent_publications;

const ALBUM: &str = "Synthetic Signals";
const RAW_ERROR: &str = "§~synthetic-mirror-error~!";
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

async fn read_completion(
    db: &Client,
    scope: &CompletionScope<'_>,
) -> Result<Option<CommittedOutcome>, PublicationError> {
    Ok(db
        .query_opt(READ, &[&&scope_key(scope)[..]])
        .await?
        .map(|row| CommittedOutcome {
            map: row.get(0),
            object: row.get(1),
            generation: row.get(2),
            configuration: row.get(3),
            root_version: row.get(4),
            media_version: row.get(5),
            page_versions: row.get(6),
        }))
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
    let assert_counter = |result: &Result<StageOutcome, PublicationError>, labels| {
        let counter = serde_json::to_value(matcher_counter(result)).unwrap();
        assert_eq!(
            counter,
            json!({
                "name": "nntune_ingest_admin_match_attempts_total",
                "labels": labels,
                "increment": 1,
            })
        );
        // Inspect the complete output, including metric and label names.
        let wire = counter.to_string();
        for marker in [job.job_id, job.owner_id, ALBUM, RELEASE, RAW_ERROR]
            .into_iter()
            .chain(TRACKS)
            .chain(both.iter().flat_map(|flac| {
                let locator = flac.receipt.locator;
                let S3Binding::VersionId(version) = locator.binding() else {
                    panic!("publication fixture requires a versioned locator");
                };
                [locator.bucket(), locator.key(), version.as_str()]
                    .into_iter()
                    .chain(flac.receipt.page_versions.iter().copied())
            }))
        {
            assert!(!wire.contains(marker));
        }
        if let Ok(StageOutcome::Staged(ids)) = result {
            assert!(ids.iter().all(|id| !wire.contains(id)));
        }
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
                    let outcome = read_completion(db, scope).await?;
                    if matches!(media, Indeterminate(_)) {
                        return Err(PublicationError::Indeterminate);
                    }
                    Ok(outcome.map(|mut outcome| {
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
    let malformed = format!(r#"{{"error":"{RAW_ERROR}","recordings":[}}"#);
    for recordings in [(503, RAW_ERROR), (200, malformed.as_str())] {
        let candidates = CompleteReleaseCandidates {
            lookup: &lookup,
            hits: vec![hit(RELEASE)],
        };
        let result = stage(&mut tx, &job, &both, ALBUM, Some(candidates), recordings).await;
        assert_eq!(
            result,
            Ok(StageOutcome::NeedsReview(NeedsReview::RecordingEvidence))
        );
        assert_counter(
            &result,
            json!({"outcome": "needs_review", "reason": "recording_evidence"}),
        );
    }
    for (rival, second_disc, reason, code) in [
        (
            true,
            2,
            NeedsReview::ReleaseUnresolved,
            "release_unresolved",
        ),
        (false, 1, NeedsReview::PositionMismatch, "position_mismatch"),
    ] {
        let result = apply(&mut tx, Receipt(&both), rival, second_disc).await;
        assert_eq!(result, Ok(StageOutcome::NeedsReview(reason)));
        assert_counter(&result, json!({"outcome": "needs_review", "reason": code}));
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
        let result = apply(&mut tx, Receipt(media), false, 2).await;
        assert_eq!(result, Err(PublicationError::Refused));
        assert_counter(&result, json!({"outcome": "refused"}));
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
    let fingerprints = [[0x83; 32], [0x84; 32]];
    let uploads = std::array::from_fn::<_, 2, _>(|i| VerifiedUpload {
        scope: CompletionScope {
            environment,
            library,
            principal: job.owner_id,
            session: &sessions[i],
            fingerprint: &fingerprints[i],
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
        Scoped(&[]),
        Scoped(&uploads[..1]),
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
    let result = apply(&mut tx, Indeterminate(&uploads), false, 2).await;
    assert_eq!(result, Err(PublicationError::Indeterminate));
    assert_counter(&result, json!({"outcome": "indeterminate"}));
    assert!(tx.query(ROWS, &[&RELEASE]).await.unwrap().is_empty());

    // Simulate a lost acknowledgement on either side of COMMIT. One retry
    // stages after rollback, and one replays the already committed rows.
    assert!(matches!(
        apply(&mut tx, Scoped(&uploads), false, 2).await,
        Ok(StageOutcome::Staged(_))
    ));
    tx.rollback().await.unwrap();

    independent_publications::exercise(db, writer, objects, &uploads).await;

    let mut tx = writer.transaction().await.unwrap();
    let result = apply(&mut tx, Scoped(&uploads), false, 2).await;
    assert_counter(&result, json!({"outcome": "staged"}));
    let StageOutcome::Staged(ids) = result.unwrap() else {
        panic!("exact multi-disc match must stage both tracks");
    };
    assert!(db.query(ROWS, &[&RELEASE]).await.unwrap().is_empty());
    tx.commit().await.unwrap();
    let mut tx = writer.transaction().await.unwrap();
    let replay = apply(&mut tx, Scoped(&uploads), false, 2).await;
    assert_eq!(replay, Ok(StageOutcome::Staged(ids.clone())));
    assert_counter(&replay, json!({"outcome": "staged"}));
    tx.commit().await.unwrap();

    // Use a dedicated one-track release so both uploads match a complete
    // recording list and the committed replacement leaves other fixtures intact.
    let replacement_release = "ab370000-0000-4000-8000-000000000000";
    let recordings = json!({
        "release_mbid": replacement_release,
        "recordings": [{
            "release_mbid": replacement_release, "mbid": TRACKS[0], "title": "Synthetic FLAC",
            "medium_no": 1, "track_no": 1
        }]
    })
    .to_string();
    let replace =
        async |tx: &mut Transaction<'_>, index: usize, job_id, error: Option<&PublicationError>| {
            let file = ExpectedFile {
                numbering: files[0].numbering,
                object: &objects[index],
            };
            let job = AuthorizedRelease {
                job_id,
                files: std::slice::from_ref(&file),
                ..job
            };
            stage_completed_release(
                tx,
                &job,
                &uploads[index..=index],
                ALBUM,
                Some(CompleteReleaseCandidates {
                    lookup: &lookup,
                    hits: vec![hit(replacement_release)],
                }),
                (200, &recordings),
                async |scope| {
                    let mut outcome = read_completion(db, scope).await?.unwrap();
                    match error {
                        Some(PublicationError::Indeterminate) => {
                            return Err(PublicationError::Indeterminate);
                        }
                        Some(PublicationError::Refused) => outcome.media_version.push('!'),
                        None => {}
                    }
                    Ok(Some(outcome))
                },
            )
            .await
        };
    const POINTER: &str = "SELECT t.id::text, s.source_job_id, l.s3_version_id
        FROM public.tracks t
        JOIN public.catalogue_s3_sources s ON s.track_id = t.id
        JOIN public.storage_s3_locator_intents l USING (object_id)
        WHERE t.release_id = $1::text::uuid";
    let pointer = |row: tokio_postgres::Row| {
        (
            row.get::<_, String>(0),
            row.get::<_, String>(1),
            row.get::<_, String>(2),
        )
    };
    let observed = async || {
        pointer(
            db.query_one(POINTER, &[&replacement_release])
                .await
                .unwrap(),
        )
    };

    let mut tx = writer.transaction().await.unwrap();
    let StageOutcome::Staged(ids) = replace(&mut tx, 1, job.job_id, None).await.unwrap() else {
        panic!("the original upload must stage one track");
    };
    let [track] = ids.as_slice() else {
        panic!("the original upload must stage one track");
    };
    tx.commit().await.unwrap();
    let expected = |index: usize, job_id: &str| {
        let S3Binding::VersionId(version) = both[index].receipt.locator.binding() else {
            panic!("the upload must have an immutable version");
        };
        (track.clone(), job_id.to_owned(), version.clone())
    };
    let old = expected(1, job.job_id);
    let new = expected(0, "catalogue-new");
    assert_ne!(old.2, new.2);
    assert_eq!(observed().await, old);

    let mut tx = writer.transaction().await.unwrap();
    for error in [PublicationError::Indeterminate, PublicationError::Refused] {
        // Compare within the writer transaction so even uncommitted writes fail.
        let result = replace(&mut tx, 0, "catalogue-new", Some(&error)).await;
        assert_eq!(result, Err(error));
        assert_eq!(
            pointer(
                tx.query_one(POINTER, &[&replacement_release])
                    .await
                    .unwrap()
            ),
            old
        );
    }
    tx.commit().await.unwrap();
    assert_eq!(observed().await, old);

    let mut tx = writer.transaction().await.unwrap();
    let replacement = replace(&mut tx, 0, "catalogue-new", None).await.unwrap();
    assert_eq!(replacement, StageOutcome::Staged(vec![track.clone()]));
    assert_eq!(
        pointer(
            tx.query_one(POINTER, &[&replacement_release])
                .await
                .unwrap()
        ),
        new
    );
    assert_eq!(observed().await, old);
    tx.commit().await.unwrap();
    assert_eq!(observed().await, new);

    let publication = async || {
        db.query_one(
            "SELECT jsonb_agg(jsonb_build_array(t, s, l) ORDER BY t.medium_no, t.track_no)::text
             FROM public.tracks t
             LEFT JOIN public.catalogue_s3_sources s ON t.id = s.track_id
             LEFT JOIN public.storage_s3_locator_intents l USING (object_id)
             WHERE t.release_id = $1::text::uuid",
            &[&RELEASE],
        )
        .await
        .unwrap()
        .get::<_, String>(0)
    };
    let before = publication().await;
    let locator = nntune_core::s3_locator::S3Locator::new(
        both[1].receipt.locator.bucket().into(),
        both[1].receipt.locator.key().into(),
        S3Binding::VersionId("synthetic-stale-media-version".into()),
    )
    .unwrap();
    let mut stale = both;
    stale[1].receipt.locator = &locator;
    // PostgreSQL makes any attempt to stage a track fail in this transaction.
    // The receipt check must refuse the second file before staging either one.
    let mut tx = writer
        .build_transaction()
        .read_only(true)
        .start()
        .await
        .unwrap();
    assert_eq!(
        apply(&mut tx, Receipt(&stale), false, 2).await,
        Err(PublicationError::Refused),
        "a version-only mismatch must refuse before catalogue staging"
    );
    assert_eq!(
        apply(&mut tx, Receipt(&both), false, 2).await,
        Err(PublicationError::Indeterminate),
        "the exact receipt must reach the write fence"
    );
    tx.commit().await.unwrap();
    assert_eq!(publication().await, before);

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
    let release = "ab340000-0000-4000-8000-000000000000";
    let rows = async || db.query(ROWS, &[&release]).await.unwrap().len();
    let stored_review = async || {
        let row = db
            .query_one(
                "SELECT view::text, retried_revision FROM public.test_flac_review",
                &[],
            )
            .await
            .unwrap();
        let view: JobView = serde_json::from_str(row.get(0)).unwrap();
        (view, row.get::<_, Option<i64>>(1))
    };
    enum RecordFailure {
        Callback,
        Database,
    }
    let run = async |client: &mut Client,
                     scope,
                     revision,
                     resolved: Option<u32>,
                     bad_receipt,
                     match_found: bool,
                     record_failure| {
        use nntune_ingest::caller_auth::issue;
        const KEY: &str = "synthetic-review-receiver-fixture-key";
        let bearer = format!("Bearer {}", issue(job.owner_id, scope, 10, KEY.as_bytes()));
        let body = format!(r#"{{"revision":{revision}}}"#);
        let recordings = include_str!("../fixtures/review_retry_recordings.json");
        retry_completed(
            &Authorization {
                bearer: Some(&bearer),
                secret: Some(KEY),
                now: 1,
            },
            client,
            ReviewTarget {
                job_id: job.job_id,
                body: body.as_bytes(),
                environment,
                library,
            },
            async |tx, actor, target| {
                assert_eq!(actor, job.owner_id);
                assert_eq!((target.environment, target.library), (environment, library));
                let row = tx.query_one("SELECT view::text, retried_revision FROM public.test_flac_review FOR UPDATE", &[]).await.unwrap();
                Ok(Some(ReviewJob {
                    view: serde_json::from_str(row.get(0)).unwrap(),
                    owner_id: job.owner_id.into(),
                    object: Some(objects[0].clone()),
                    retried_revision: row.get::<_, Option<i64>>(1).map(|prior| prior as u32),
                }))
            },
            async |tx, expected, result| {
                assert!(resolved.is_some(), "replay must not record a new outcome");
                let view = serde_json::to_string(result).unwrap();
                let prior = i64::from(expected.view.revision);
                let changed = tx.execute("UPDATE public.test_flac_review SET view = $1::text::jsonb,
                    retried_revision = $2 WHERE (view->>'revision')::bigint = $2", &[&view, &prior]).await.unwrap();
                assert_eq!(changed, 1);
                if result.status == ReviewStatus::Published {
                    assert_eq!(tx.query(ROWS, &[&release]).await.unwrap().len(), 1);
                    assert_eq!(rows().await, 0, "staged track must be invisible to readers");
                    let (stored_view, retried_revision) = stored_review().await;
                    assert_eq!(
                        (&stored_view, retried_revision),
                        (&expected.view, expected.retried_revision.map(i64::from)),
                        "staged receipt must be invisible to readers"
                    );
                }
                match record_failure {
                    Some(RecordFailure::Database) => {
                        // Force a database error after staging the track and receipt.
                        let error = tx
                            .execute("UPDATE public.test_flac_review SET view = NULL", &[])
                            .await
                            .expect_err("the receipt must reject a null view");
                        assert_eq!(error.code(), Some(&SqlState::NOT_NULL_VIOLATION));
                        Err(BoundaryError::Unavailable)
                    }
                    Some(RecordFailure::Callback) => Err(BoundaryError::Unavailable),
                    None => Ok(()),
                }
            },
            async |_| {
                Ok(ResolvedCompletion {
                    job_id: job.job_id,
                    revision: resolved.expect("replay must not resolve the upload"),
                    upload: uploads[usize::from(bad_receipt)],
                    numbering: files[0].numbering,
                    query: ALBUM,
                    recordings: (200, recordings),
                    candidates: match_found.then(|| CompleteReleaseCandidates {
                        lookup: &lookup,
                        hits: vec![hit(release)],
                    }),
                })
            },
            async |scope| {
                assert!(resolved.is_some(), "replay must not reread upload evidence");
                Ok(db
                    .query_opt(READ, &[&&scope_key(scope)[..]])
                    .await?
                    .map(|row| CommittedOutcome {
                        map: row.get(0),
                        object: row.get(1),
                        generation: row.get(2),
                        configuration: row.get(3),
                        root_version: row.get(4),
                        media_version: row.get(5),
                        page_versions: row.get(6),
                    }))
            },
        )
        .await
    };
    for (scope, revision, resolved, bad_receipt, record_failure, status) in [
        (READ_SCOPE, 1, 1, false, None, 404),
        (RETRY_SCOPE, 0, 1, false, None, 409),
        (RETRY_SCOPE, 1, 2, false, None, 409),
        (RETRY_SCOPE, 1, 1, true, None, 409),
        (RETRY_SCOPE, 1, 1, false, Some(RecordFailure::Callback), 503),
    ] {
        let error = run(
            writer,
            scope,
            revision,
            Some(resolved),
            bad_receipt,
            true,
            record_failure,
        )
        .await
        .unwrap_err();
        assert_eq!(error.status, status);
        assert_eq!(rows().await, 0);
    }
    let review = run(writer, RETRY_SCOPE, 1, Some(1), false, false, None)
        .await
        .unwrap();
    assert_eq!(review.status, ReviewStatus::NeedsReview);
    assert_eq!(review.revision, 2);
    assert_eq!(review.reason, Some(NeedsReview::SearchEvidence));
    let replay = run(writer, RETRY_SCOPE, 1, None, true, true, None)
        .await
        .unwrap();
    assert_eq!(replay, review);
    assert_eq!(rows().await, 0);

    let receipt = &both[0].receipt;
    let S3Binding::VersionId(version) = receipt.locator.binding() else {
        panic!("fixture must pin an immutable media version");
    };
    let publication = async || {
        // Read from the canonical track side so a duplicate track or a missing
        // source cannot be hidden by filtering for the expected object.
        let row = db
            .query_one(
                "SELECT t.id::text AS track_id, s.source_job_id, s.accepted_seq,
                        s.object_id::text, m.map_id::text, l.s3_bucket, l.s3_key,
                        l.s3_version_id, p.media_version, p.page_versions, r.view::text, r.retried_revision
                 FROM public.tracks t
                 LEFT JOIN public.catalogue_s3_sources s ON s.track_id = t.id
                 LEFT JOIN public.storage_s3_locator_intents l USING (object_id)
                 LEFT JOIN public.storage_digest_map_intents m USING (object_id)
                 LEFT JOIN public.storage_digest_inventory_intents p USING (map_id)
                 CROSS JOIN public.test_flac_review r
                 WHERE t.release_id = $1::text::uuid",
                &[&release],
            )
            .await
            .unwrap();
        assert_eq!(row.get::<_, &str>("source_job_id"), job.job_id);
        assert_eq!(
            row.get::<_, String>("object_id").replace('-', ""),
            hex::encode(receipt.intent.identities[2]),
            "publication must still point at the committed object"
        );
        assert_eq!(
            row.get::<_, String>("map_id").replace('-', ""),
            hex::encode(receipt.intent.map_id)
        );
        assert_eq!(row.get::<_, &str>("s3_bucket"), receipt.locator.bucket());
        assert_eq!(row.get::<_, &str>("s3_key"), receipt.locator.key());
        assert_eq!(row.get::<_, &str>("s3_version_id"), version);
        assert_eq!(row.get::<_, &str>("media_version"), version);
        assert_eq!(
            row.get::<_, Vec<String>>("page_versions"),
            receipt.page_versions
        );
        assert_eq!(row.get::<_, i64>("retried_revision"), 2);
        let view: JobView = serde_json::from_str(row.get("view")).unwrap();
        assert_eq!(view.job_id, job.job_id);
        assert_eq!((view.status, view.revision), (ReviewStatus::Published, 3));
        (
            row.get::<_, String>("track_id"),
            row.get::<_, i64>("accepted_seq"),
            view,
        )
    };

    // The same committed upload and review revision must recover after rollback.
    let failure = run(
        outcome_writer,
        RETRY_SCOPE,
        2,
        Some(2),
        false,
        true,
        Some(RecordFailure::Database),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.status, 503);
    assert_eq!(rows().await, 0);
    assert_eq!(stored_review().await, (review, Some(1)));

    let first = run(writer, RETRY_SCOPE, 2, Some(2), false, true, None)
        .await
        .unwrap();
    let (track_id, accepted_seq, view) = publication().await;
    assert_eq!(
        first, view,
        "first response must match the committed receipt"
    );

    // Discard the response after checking it at the receiver. The retry
    // still has only the original revision and a separate connection.
    drop(first);
    let replay = run(outcome_writer, RETRY_SCOPE, 2, None, true, false, None)
        .await
        .unwrap();
    assert_eq!(replay, view);
    assert_eq!(publication().await, (track_id, accepted_seq, view));
}
