use nntune_core::media_envelope::EnvelopeContext;
use tokio_postgres::Transaction;

use super::now;
use crate::{
    admin_auto_publish::{
        completed_upload::{stage_completed_release, VerifiedUpload},
        AuthorizedRelease, CommittedFlac, ExpectedFile, StageOutcome,
    },
    admin_review::ObjectRef,
    catalogue_publication::{
        stage_with_duration, PublicationError, PublishedObject, VerifiedOccurrence,
    },
    digest_candidate_store::{
        outcome::{read_completion, stage_completion},
        CandidateIntent,
    },
    matching::{
        admin_publish_match::{release_lookup, CompleteReleaseCandidates, FileNumbering},
        MbSearchHit,
    },
    multipart::{
        readback_inventory::{ReadbackScope, VerifiedCompletedReadback},
        session_outcome::CompletionScope,
    },
    track_writers::TrackRow,
};

#[path = "catalogue_postgres_wasm.rs"]
mod postgres;

const OWNER: &str = "catalogue-owner";
const SESSION: &str = "99000000-0000-4000-8000-000000000001";
const LEASE: &str = "99000000-0000-4000-8000-000000000002";
const ALBUM: &str = "Synthetic Signals";
const RELEASE: &str = "ab340000-0000-4000-8000-000000000000";

pub(super) async fn publish(
    verified: &VerifiedCompletedReadback<'_, '_>,
    context: &EnvelopeContext,
    record: &[u8],
    map_id: [u8; 16],
    object: ObjectRef,
    upload_id: &str,
) -> String {
    let [root, environment, library, object_id, generation, configuration] = context.identities;
    let ids = context.identities.map(hex::encode);
    let inventory = verified.inventory();
    let intent = CandidateIntent {
        identities: [environment, library, object_id, generation, configuration],
        plaintext_len: context.plaintext_len,
        map_id,
        header: context.header,
        reservation_generation: 1,
    };
    let hashes = verified
        .origin()
        .candidate_page_hashes(
            &intent.identities,
            intent.plaintext_len,
            &map_id,
            &intent.header,
        )
        .unwrap();
    let bucket = inventory.bucket();
    let offset = i64::try_from(intent.plaintext_len).unwrap();
    let sealed = i64::try_from(inventory.geometry().sealed_len()).unwrap();
    let scope = CompletionScope {
        environment,
        library,
        principal: OWNER,
        session: SESSION,
        fingerprint: &[0xfa; 32],
        seal: generation,
        revision: 1,
        offset,
    };
    let mut writer = postgres::connect().await;
    writer
        .batch_execute(concat!(
            include_str!("../../../api/tests/fixtures/storage_s3_locator_schema.sql"),
            include_str!("catalogue_source_schema.sql"),
            include_str!("auto_publish_candidate.sql"),
            include_str!("catalogue_upload_wasm.sql"),
        ))
        .await
        .unwrap();
    writer
        .execute(
            "INSERT INTO public.storage_environments VALUES ($1::text::uuid)",
            &[&ids[1]],
        )
        .await
        .unwrap();
    writer.execute(
        "INSERT INTO public.storage_library_namespaces VALUES ($1::text::uuid, $2::text::uuid, NULL)",
        &[&ids[2], &ids[1]],
    ).await.unwrap();
    writer
        .query_one(
            "SELECT pg_temp.prepare_catalogue_candidate($1, $2, $3, $4, $5, $6, $7, $8)",
            &[
                &ids.as_slice(),
                &hex::encode(map_id),
                &offset,
                &sealed,
                &intent.header.as_slice(),
                &object.plaintext_sha256.as_slice(),
                &hashes[0].as_slice(),
                &record,
            ],
        )
        .await
        .unwrap();
    writer.execute(
        "INSERT INTO public.ingest_session_reservations
         (id, library_id, principal_id, idempotency_key, fingerprint, absolute_deadline,
          idle_deadline, checkpoint_generation, checkpoint_revision, checkpoint_offset,
          write_lease_id, write_lease_generation, multipart_object_id, multipart_bucket, multipart_upload_id)
         VALUES ($1::text::uuid, $2::text::uuid, $3, 'synthetic-upload', $4,
                 statement_timestamp() + interval '5 minutes', statement_timestamp() + interval '5 minutes',
                 $5::text::uuid, 1, $6, $7::text::uuid, $5::text::uuid, $8::text::uuid, $9, $10)",
        &[&SESSION, &ids[2], &OWNER, &scope.fingerprint.as_slice(), &ids[4],
          &offset, &LEASE, &ids[3], &bucket, &upload_id],
    ).await.unwrap();

    let retain_until = i64::try_from(
        now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
        + 600;
    let mut tx = writer.transaction().await.unwrap();
    stage_completion(
        &mut tx,
        &scope,
        LEASE,
        &intent,
        ReadbackScope {
            bucket,
            configuration,
        },
        verified,
        retain_until,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut outcomes = postgres::connect().await;
    let committed = read_completion(&mut outcomes, &scope, async |_, _| Ok(retain_until))
        .await
        .unwrap()
        .unwrap();
    assert!(verified.matches_committed_outcome(&committed));
    let seek = verified
        .flac_plaintext_range(&object.media, &object.plaintext_sha256, 5020..5021)
        .expect("the committed tone has a later seek anchor");

    // Install the rehearsal storage locator from the committed outcome. The
    // production publication writer owns the track and source projections.
    writer
        .execute(
            "INSERT INTO public.storage_s3_locator_intents
         VALUES ($1::text::uuid, $2::text::uuid, $3, $4, $5, NULL)",
            &[
                &committed.object,
                &committed.root_version,
                &bucket,
                &object.media.locator().key(),
                &committed.media_version,
            ],
        )
        .await
        .unwrap();
    let page_versions: Vec<_> = committed.page_versions.iter().map(String::as_str).collect();
    let upload = VerifiedUpload {
        scope,
        flac: CommittedFlac {
            verified,
            receipt: PublishedObject {
                intent: &intent,
                locator: object.media.locator(),
                plaintext_sha256: &object.plaintext_sha256,
                root_version_id: root,
                page_versions: &page_versions,
            },
        },
    };
    let job = AuthorizedRelease {
        job_id: "catalogue-old",
        owner_id: OWNER,
        environment,
        library,
        files: &[ExpectedFile {
            numbering: FileNumbering {
                disc: Some("1"),
                track: Some("1"),
            },
            object: &object,
        }],
    };
    let lookup = release_lookup(ALBUM).unwrap();
    let mut tx = writer.transaction().await.unwrap();
    let occurrence = VerifiedOccurrence {
        job_id: job.job_id,
        owner_id: OWNER,
        object_id,
        track: TrackRow {
            release_id: RELEASE,
            title: "Fixture",
            medium_no: 1,
            track_no: 1,
            mbid: "ab340000-0000-4000-8000-000000000001",
            file_ordinal: 0,
        },
    };
    for measured_ms in [i32::MAX as u64 + 1, u64::from(u32::MAX) + 2] {
        assert_eq!(
            stage_with_duration(
                &mut tx,
                &occurrence,
                &upload.flac.receipt,
                Some(measured_ms)
            )
            .await,
            Err(PublicationError::Refused),
        );
    }
    let mut stage_upload = async |tx: &mut Transaction<'_>, expected_version: Option<&str>| {
        let candidates = CompleteReleaseCandidates {
            lookup: &lookup,
            hits: vec![MbSearchHit {
                mbid: RELEASE.into(),
                title: ALBUM.into(),
                artist_name: None,
                artist_mbid: None,
                score: 0.9,
            }],
        };
        stage_completed_release(
            tx,
            &job,
            &[upload],
            ALBUM,
            Some(candidates),
            (200, include_str!("review_retry_recordings.json")),
            async |scope| {
                let mut outcome =
                    read_completion(&mut outcomes, scope, async |_, _| Ok(retain_until))
                        .await
                        .map_err(|_| PublicationError::Indeterminate)?
                        .expect("the scoped upload must be completed");
                if let Some(version) = expected_version {
                    assert_ne!(outcome.media_version, version);
                    outcome.media_version = version.into();
                }
                Ok(Some(outcome))
            },
        )
        .await
    };
    // Keep the authenticated upload and its locator intact. A different
    // version in the expected receipt must stop the handoff before staging.
    assert_eq!(
        stage_upload(&mut tx, Some("synthetic-other-media-version")).await,
        Err(PublicationError::Refused)
    );
    assert!(tx
        .query_one(
            "SELECT NOT EXISTS (SELECT FROM public.tracks WHERE release_id = $1::text::uuid)",
            &[&RELEASE],
        )
        .await
        .unwrap()
        .get::<_, bool>(0));
    let StageOutcome::Staged(tracks) = stage_upload(&mut tx, None).await.unwrap() else {
        panic!("expected the committed FLAC to publish");
    };
    let [track] = tracks.as_slice() else {
        panic!("expected one canonical track");
    };
    tx.commit().await.unwrap();

    serde_json::json!({
        "track": track,
        "mediaVersion": committed.media_version,
        "seek": seek,
    })
    .to_string()
}
