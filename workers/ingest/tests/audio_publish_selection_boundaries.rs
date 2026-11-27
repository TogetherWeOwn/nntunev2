//! Offline coverage for dormant audio acceptance and publication boundaries.

use std::future::pending;

use futures_util::{future::LocalBoxFuture, stream, stream::LocalBoxStream, StreamExt};
use nntune_core::s3_locator::{S3Binding, S3Locator};
use nntune_ingest::{
    audio_acceptance::{accept_manifest, AcceptanceError, ManifestAcceptanceStore},
    audio_classifications::{classify_entries, ClassificationCheckpoint},
    audio_manifest::AcceptedAudioManifest,
    audio_manifest_store::FreezeError,
    audio_positions::prepare_all_local_occurrences,
    audio_publication::{prepare_publication, PublicationError},
    audio_scratch::{ScratchAudioReader, ScratchPrefixResponse, ScratchPrefixStore},
    audio_selection::{
        extract_hints, AudioSelectionPlan, DirectAudioSelection, PrefixReader, SelectionError,
    },
    catalogue_publication::{PublicationError as CatalogueError, PublishedObject},
    digest_candidate_store::CandidateIntent,
    dispatch::content_sha256_hex,
    verify::{StagedFile, VerifyProgress},
};
use nntune_test_nntp::{nzb_for, Article};
use serde_json::{json, Value};

const JOB: &str = "audio-boundaries";
const OWNER: &str = "fixture-owner";
fn original() -> String {
    let article = Article {
        message_id: "<part1@example.invalid>".into(),
        number: 1,
        group: "alt.binaries.test".into(),
        subject: "\"a.flac\" yEnc (1/1)".into(),
        from: String::new(),
        date: String::new(),
        raw: vec![0; 100],
        payload: vec![],
    };
    nzb_for(&[&article], &article.group)
}

fn manifest(hints: [Value; 2], frozen: bool) -> AcceptedAudioManifest {
    let entries: Vec<_> = hints
        .into_iter()
        .enumerate()
        .map(|(index, hints)| {
            json!({
                "audio_ordinal": index + 1,
                "source_selector": {
                    "nzb_file_ordinal": index, "archive_member": null, "audio_selection_index": 0
                },
                "artist_group_ref": 1, "release_group_ref": 1, "hints": hints
            })
        })
        .collect();
    let mut record = json!({
        "version": 1, "job_id": JOB, "user_id": OWNER,
        "original_content_sha256": "a".repeat(64),
        "entries": entries, "art_blobs": []
    });
    if frozen {
        record["effective_discs"] = json!([1, 1]);
    }
    AcceptedAudioManifest::parse(&record.to_string()).unwrap()
}

struct AcceptanceStore(Option<AcceptedAudioManifest>);

impl ManifestAcceptanceStore for AcceptanceStore {
    async fn load(
        &self,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<Option<AcceptedAudioManifest>, FreezeError> {
        Ok(self.0.clone())
    }

    async fn freeze(
        &self,
        _: &AcceptedAudioManifest,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<AcceptedAudioManifest, FreezeError> {
        panic!("an unfrozen candidate must never reach the store")
    }
}

#[tokio::test]
async fn fresh_unfrozen_candidate_is_refused_and_legacy_winner_replays() {
    let legacy = manifest([json!({}), json!({})], false);
    let hash = legacy.original_content_sha256();
    assert_eq!(
        accept_manifest(&AcceptanceStore(None), JOB, OWNER, hash, || async {
            Ok(legacy.clone())
        })
        .await,
        Err(AcceptanceError::UnfrozenCandidate)
    );
    assert_eq!(
        accept_manifest(
            &AcceptanceStore(Some(legacy.clone())),
            JOB,
            OWNER,
            hash,
            || async { panic!("legacy winner must replay without preparation") }
        )
        .await,
        Ok(legacy.clone())
    );
}

#[test]
fn zero_duration_and_blank_fingerprints_cannot_collapse_tracks() {
    for hints in [
        json!({"title": "A", "track_no": 7, "duration_ms": 0}),
        json!({"track_no": 7, "fingerprint": "   "}),
    ] {
        let input = manifest([hints.clone(), hints], true);
        let plan = prepare_all_local_occurrences(&input).unwrap();
        let positions: Vec<_> = plan
            .entries()
            .iter()
            .map(|entry| (entry.medium_no, entry.track_no, entry.duplicate_of))
            .collect();
        assert_eq!(positions, [(1, 1, None), (1, 2, None)]);
    }
}

#[test]
fn publication_refuses_local_positions_or_duplicates_that_disagree_with_the_manifest() {
    let input = manifest([json!({"track_no": 1}), json!({"track_no": 2})], true);
    let checkpoint = classify_entries(&input, &[None, None]).unwrap();
    assert_eq!(
        prepare_publication(&input, &checkpoint)
            .unwrap()
            .entries()
            .len(),
        2
    );
    for (track_no, duplicate_of) in [(3, None), (1, Some(1))] {
        let mut record = serde_json::to_value(&checkpoint).unwrap();
        record["entries"][1]["track_no"] = json!(track_no);
        record["entries"][1]["duplicate_of"] = json!(duplicate_of);
        let inconsistent = ClassificationCheckpoint::parse(&record.to_string()).unwrap();
        assert_eq!(
            prepare_publication(&input, &inconsistent),
            Err(PublicationError::Positions)
        );
    }
}

struct EmptyPrefix;

impl PrefixReader for EmptyPrefix {
    fn prefix<'a>(
        &'a mut self,
        _: &'a DirectAudioSelection,
        _: usize,
    ) -> LocalBoxStream<'a, Result<Vec<u8>, ()>> {
        stream::iter([Ok(Vec::new())]).boxed_local()
    }
}

#[tokio::test]
async fn empty_prefix_does_not_produce_a_hintless_candidate() {
    let original = original();
    let hash = content_sha256_hex(original.as_bytes());
    let plan = AudioSelectionPlan::from_original(&original, &hash).unwrap();
    assert_eq!(
        extract_hints(&plan, &mut EmptyPrefix).await.err(),
        Some(SelectionError::Transport)
    );
}

struct Scratch(&'static [u8]);

impl ScratchPrefixStore for Scratch {
    fn read<'a>(
        &'a self,
        expected: &'a StagedFile,
        length: usize,
    ) -> LocalBoxFuture<'a, Result<ScratchPrefixResponse<'a>, ()>> {
        Box::pin(async move {
            Ok(ScratchPrefixResponse {
                staged: expected.clone(),
                object_size: expected.size,
                range_offset: 0,
                range_length: length as u64,
                body: stream::iter([Ok(self.0.to_vec())]).boxed_local(),
            })
        })
    }
}

#[tokio::test]
async fn full_scratch_read_must_match_the_verified_digest() {
    let original = original();
    let hash = content_sha256_hex(original.as_bytes());
    let plan = AudioSelectionPlan::from_original(&original, &hash).unwrap();
    let mut progress = VerifyProgress::new(1);
    progress.staged.insert(
        0,
        StagedFile {
            key: VerifyProgress::file_key(JOB, 1, 0, "a.flac"),
            size: 4,
            sha256: content_sha256_hex(b"fLaC"),
            missing_blocks: 0,
            size_refused_blocks: 0,
            par2_set_id: None,
            spool: vec![],
            crc32: 0,
            missing: false,
        },
    );
    for (bytes, expected) in [(b"fLaC", None), (b"fLaX", Some(SelectionError::Transport))] {
        let mut reader =
            ScratchAudioReader::bind(JOB, &original, &hash, &progress, Scratch(bytes), pending())
                .unwrap();
        assert_eq!(extract_hints(&plan, &mut reader).await.err(), expected);
    }
    progress.repair_set_ids.push("a".repeat(32));
    assert_eq!(
        ScratchAudioReader::bind(
            JOB,
            &original,
            &hash,
            &progress,
            Scratch(b"fLaC"),
            pending()
        )
        .err(),
        Some(SelectionError::Identity)
    );
}

#[test]
fn catalogue_receipt_requires_exact_page_count_and_real_version_pins() {
    let key_id = "00000000000040008000000000000001";
    let validate_receipt = |plaintext_len, page_versions: &[&str], media_version: &str| {
        let locator = S3Locator::new(
            "synthetic-media".into(),
            format!("media/{key_id}/{key_id}"),
            S3Binding::VersionId(media_version.into()),
        )
        .unwrap();
        let intent = CandidateIntent {
            identities: [[0; 16]; 5],
            plaintext_len,
            map_id: [0; 16],
            header: [0; 48],
            reservation_generation: 1,
        };
        PublishedObject {
            intent: &intent,
            locator: &locator,
            plaintext_sha256: &[0; 32],
            root_version_id: [0; 16],
            page_versions,
        }
        .version_pin()
        .map(|_| ())
    };
    let validate = |length, pins: &[&str]| validate_receipt(length, pins, "media-version");
    let one_page = 256 * 65_536;
    assert_eq!(validate(one_page, &["page-one"]), Ok(()));
    assert_eq!(validate(one_page + 1, &["page-one", "page-two"]), Ok(()));
    assert_eq!(
        validate(one_page + 1, &["page-one"]),
        Err(CatalogueError::Refused)
    );
    assert_eq!(validate(0, &["page-one"]), Err(CatalogueError::Refused));
    assert_eq!(
        validate(u64::MAX, &["page-one"]),
        Err(CatalogueError::Refused)
    );
    for pin in ["", "null", "two words", "控制", "pin-one,pin-two"] {
        assert_eq!(validate(one_page, &[pin]), Err(CatalogueError::Refused));
    }
    assert_eq!(
        validate_receipt(one_page, &["page-one"], "pin-one,pin-two"),
        Err(CatalogueError::Refused)
    );
}
