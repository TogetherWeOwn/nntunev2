use std::{cell::Cell, future::pending};

use futures_util::future::LocalBoxFuture;
use nntune_core::digest_enrollment::DigestEnrollment;
use nntune_ingest::{
    audio_manifest::AcceptedAudioManifest,
    audio_mirror_evidence::{
        derive_evidence, mirror_snapshot_digest, EvidenceError, MirrorRecording, MirrorSnapshot,
    },
    audio_producer::{prepare_verified_candidate, ProducerError, VerifiedCandidateInput},
    audio_scratch::{ScratchPrefixResponse, ScratchPrefixStore},
    audio_selection::SelectionError,
    digest_candidate_identity::{candidate_hashes, CandidateError, CandidateIntent},
    dispatch::content_sha256_hex,
    library::{upsert_library_sql, validate_library_write},
    matching::MbRecording,
    queue::LibraryWrite,
    track_writers::derive_track_rows,
    verify::{StagedFile, VerifyProgress},
};
use serde_json::json;

const JOB: &str = "boundary_job";
const OWNER: &str = "boundary_owner";
const RELEASE: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const RECORDING: &str = "bbbbbbbb-2222-4222-8222-222222222222";
const OTHER_RECORDING: &str = "cccccccc-3333-4333-8333-333333333333";
const NZB: &str = r#"<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
<file poster="test" date="0" subject="&quot;one.flac&quot; yEnc (1/1)">
<groups><group>alt.binaries.test</group></groups>
<segments><segment bytes="100" number="1">part0@example.invalid</segment></segments>
</file></nzb>"#;

fn hinted_manifest() -> AcceptedAudioManifest {
    let entries = [RECORDING, OTHER_RECORDING]
        .into_iter()
        .enumerate()
        .map(|(i, recording)| {
            json!({
                "audio_ordinal": i + 1, "artist_group_ref": 1, "release_group_ref": 1,
                "source_selector": {"nzb_file_ordinal": i, "audio_selection_index": 0},
                "hints": {"release_mbid": if i == 0 {RELEASE.into()} else {RELEASE.to_uppercase()},
                          "recording_mbid": recording},
            })
        })
        .collect::<Vec<_>>();
    AcceptedAudioManifest::parse(
        &json!({
            "version": 1, "job_id": JOB, "user_id": OWNER,
            "original_content_sha256": "a".repeat(64),
            "entries": entries, "art_blobs": [], "effective_discs": [1, 1],
        })
        .to_string(),
    )
    .unwrap()
}

fn snapshot(second_position: i32) -> MirrorSnapshot {
    let recordings = [(RECORDING, 1), (OTHER_RECORDING, second_position)]
        .into_iter()
        .map(|(mbid, track_no)| MirrorRecording {
            recording_mbid: mbid.into(),
            medium_no: 1,
            track_no,
        })
        .collect::<Vec<_>>();
    MirrorSnapshot {
        release_mbid: RELEASE.into(),
        snapshot_sha256: mirror_snapshot_digest(RELEASE, &recordings),
        recordings,
    }
}

#[test]
fn mirror_refuses_snapshot_drift_across_release_case_aliases() {
    let manifest = hinted_manifest();
    assert!(derive_evidence(&manifest, |_| Some(snapshot(2))).is_ok());
    let calls = Cell::new(0);
    let evidence = derive_evidence(&manifest, |_| {
        calls.set(calls.get() + 1);
        Some(snapshot(calls.get() + 1))
    });
    assert_eq!(evidence, Err(EvidenceError::Snapshot));
}

#[test]
fn mirror_rejects_distinct_recordings_at_the_same_position() {
    assert_eq!(
        derive_evidence(&hinted_manifest(), |_| Some(snapshot(1))),
        Err(EvidenceError::Snapshot)
    );
}

struct NoScratch;

impl ScratchPrefixStore for NoScratch {
    fn read<'a>(
        &'a self,
        _: &'a StagedFile,
        _: usize,
    ) -> LocalBoxFuture<'a, Result<ScratchPrefixResponse<'a>, ()>> {
        panic!("an unfinished round must be rejected before scratch reads")
    }
}

#[tokio::test]
async fn producer_refuses_pending_multi_set_repair_with_zero_counters() {
    let mut progress = VerifyProgress::new(1);
    progress.staged.insert(
        0,
        StagedFile {
            key: VerifyProgress::file_key(JOB, 1, 0, "one.flac"),
            size: 100,
            sha256: "b".repeat(64),
            missing_blocks: 0,
            size_refused_blocks: 0,
            par2_set_id: None,
            spool: vec![],
            crc32: 0,
            missing: false,
        },
    );
    progress.repair_set_ids = vec!["a".repeat(32), "b".repeat(32)];
    let input = VerifiedCandidateInput {
        job_id: JOB,
        user_id: OWNER,
        original: NZB,
        expected_hash: &content_sha256_hex(NZB.as_bytes()),
        progress: &progress,
    };
    assert_eq!(
        prepare_verified_candidate(input, NoScratch, pending()).await,
        Err(ProducerError::Selection(SelectionError::Identity))
    );
}

#[test]
fn digest_candidate_rejects_object_sealing_generation_alias() {
    let v4 = |byte| {
        let mut id = [byte; 16];
        id[6] = 0x40;
        id[8] = 0x80;
        id
    };
    let identities = [v4(1), v4(2), v4(3), v4(3), v4(5)];
    let map_id = v4(6);
    let mut enrollment = DigestEnrollment::new(1, identities, map_id).unwrap();
    let header = enrollment.header().to_bytes();
    enrollment.seal_chunk(&[42]).unwrap();
    let manifest = enrollment.finish().ok().unwrap();
    let intent = CandidateIntent {
        identities,
        plaintext_len: 1,
        map_id,
        header,
        reservation_generation: 1,
    };
    assert_eq!(
        candidate_hashes(&intent, &manifest),
        Err(CandidateError::Identity)
    );
}

#[test]
fn library_rejects_foreign_owner_keys_and_case_aliased_recordings() {
    let mut write = LibraryWrite {
        job_id: JOB.into(),
        user_id: OWNER.into(),
        idempotency_key: format!("ingest:{OWNER}:{}:upload", "a".repeat(64)),
        release_mbid: Some(RELEASE.into()),
        recording_mbids: vec![RECORDING.to_uppercase(), OTHER_RECORDING.into()],
    };
    assert_eq!(validate_library_write(&write), Ok(()));
    write.user_id = "other_owner".into();
    assert!(validate_library_write(&write).is_err());
    write.user_id = OWNER.into();
    write.recording_mbids[1] = RECORDING.into();
    assert_eq!(
        validate_library_write(&write),
        Err("duplicate recording mbids")
    );
    assert!(upsert_library_sql().ends_with("WHERE library.user_id = EXCLUDED.user_id"));
}

#[test]
fn track_derivation_refuses_ambiguous_or_out_of_bounds_file_ordinals() {
    let document = nntune_core::nzb::parse(NZB.as_bytes()).unwrap();
    let mut files = document.files;
    let recordings = [MbRecording {
        mbid: RECORDING.into(),
        title: "One".into(),
        medium_no: Some(1),
        track_no: Some(1),
    }];
    assert!(derive_track_rows(Some(RELEASE), &files, &recordings).is_ok());
    files.push(files[0].clone());
    assert!(derive_track_rows(Some(RELEASE), &files, &recordings).is_err());
    files.pop();
    files[0].ordinal = nntune_core::nzb::MAX_FILES;
    assert!(derive_track_rows(Some(RELEASE), &files, &recordings).is_err());
    files[0].ordinal = 0;
    let oversized = vec![recordings[0].clone(); nntune_ingest::matching::MAX_RECORDING_MBIDS + 1];
    assert!(derive_track_rows(Some(RELEASE), &files, &oversized).is_err());
}
