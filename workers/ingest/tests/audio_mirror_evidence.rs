//! Mirror-evidence retrieval: hinted recordings bind only to their exact
//! digest-verified snapshot; missing or unhinted rows are all-local;
//! invalid snapshots refuse the whole derivation.
use nntune_ingest::{
    audio_manifest::AcceptedAudioManifest,
    audio_mirror_evidence::{
        derive_evidence, mirror_snapshot_digest, EvidenceError, MirrorRecording, MirrorSnapshot,
    },
};
use serde_json::json;

const JOB: &str = "job_0123456789abcdef";
const OWNER: &str = "owner-1";
const RECORDING: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const RELEASE: &str = "cccccccc-3333-4333-8333-333333333333";

fn hash() -> String {
    "a".repeat(64)
}

fn entry_ordinal(ordinal: u32, hints: serde_json::Value) -> serde_json::Value {
    json!({
        "audio_ordinal": ordinal,
        "source_selector": {
            "nzb_file_ordinal": ordinal - 1,
            "archive_member": null,
            "audio_selection_index": 0,
        },
        "artist_group_ref": 1,
        "release_group_ref": 1,
        "hints": hints,
    })
}

fn manifest_with(hints: Vec<serde_json::Value>) -> AcceptedAudioManifest {
    let entries: Vec<serde_json::Value> = hints
        .into_iter()
        .enumerate()
        .map(|(i, h)| entry_ordinal(i as u32 + 1, h))
        .collect();
    let value = json!({
        "version": 1, "job_id": JOB, "user_id": OWNER,
        "original_content_sha256": hash(),
        "entries": entries,
        "art_blobs": [],
        "effective_discs": vec![1; entries.len()],
    });
    AcceptedAudioManifest::parse(&value.to_string()).unwrap()
}

fn recording(recording_mbid: &str, medium_no: i32, track_no: i32) -> MirrorRecording {
    MirrorRecording {
        recording_mbid: recording_mbid.into(),
        medium_no,
        track_no,
    }
}

fn snapshot() -> MirrorSnapshot {
    let recordings = vec![recording(RECORDING, 1, 3)];
    let digest = mirror_snapshot_digest(RELEASE, &recordings);
    MirrorSnapshot {
        release_mbid: RELEASE.into(),
        snapshot_sha256: digest,
        recordings,
    }
}

fn hinted() -> AcceptedAudioManifest {
    manifest_with(vec![
        json!({"title": "t", "release_mbid": RELEASE, "recording_mbid": RECORDING}),
    ])
}

#[test]
fn hinted_recording_binds_to_verified_snapshot() {
    let slots = derive_evidence(&hinted(), |_| Some(snapshot())).unwrap();
    let evidence = slots[0].as_ref().unwrap();
    assert_eq!(evidence.recording_mbid, RECORDING);
    assert_eq!(evidence.release_mbid, RELEASE);
    assert_eq!(evidence.medium_no, 1);
    assert_eq!(evidence.track_no, 3);
    assert_eq!(evidence.snapshot_sha256, snapshot().snapshot_sha256);
}

#[test]
fn recording_mbid_match_is_case_insensitive() {
    let upper = RECORDING.to_ascii_uppercase();
    let manifest = manifest_with(vec![
        json!({"title": "t", "release_mbid": RELEASE, "recording_mbid": upper}),
    ]);
    let slots = derive_evidence(&manifest, |_| Some(snapshot())).unwrap();
    assert!(slots[0].is_some());
}

#[test]
fn absent_recording_is_all_local() {
    let other = "dddddddd-4444-4444-8444-444444444444";
    let manifest = manifest_with(vec![
        json!({"title": "t", "release_mbid": RELEASE, "recording_mbid": other}),
    ]);
    let slots = derive_evidence(&manifest, |_| Some(snapshot())).unwrap();
    assert_eq!(slots[0], None);
}

#[test]
fn missing_snapshot_is_all_local() {
    let slots = derive_evidence(&hinted(), |_| None).unwrap();
    assert_eq!(slots[0], None);
}

#[test]
fn unhinted_entry_is_all_local() {
    let manifest = manifest_with(vec![json!({"title": "t"})]);
    let slots = derive_evidence(&manifest, |_| Some(snapshot())).unwrap();
    assert_eq!(slots[0], None);
}

#[test]
fn recording_hint_without_release_mbid_is_all_local() {
    let manifest = manifest_with(vec![json!({"title": "t", "recording_mbid": RECORDING})]);
    let slots = derive_evidence(&manifest, |_| Some(snapshot())).unwrap();
    assert_eq!(slots[0], None);
}

#[test]
fn digest_mismatch_refuses_whole_derivation() {
    let mut bad = snapshot();
    bad.snapshot_sha256 = "0".repeat(64);
    assert_eq!(
        derive_evidence(&hinted(), |_| Some(bad)),
        Err(EvidenceError::Digest)
    );
}

#[test]
fn non_positive_snapshot_position_refuses() {
    let recordings = vec![recording(RECORDING, 0, 3)];
    let bad = MirrorSnapshot {
        snapshot_sha256: mirror_snapshot_digest(RELEASE, &recordings),
        release_mbid: RELEASE.into(),
        recordings,
    };
    assert_eq!(
        derive_evidence(&hinted(), |_| Some(bad)),
        Err(EvidenceError::Identity)
    );
}

#[test]
fn conflicting_duplicate_recording_refuses() {
    let recordings = vec![recording(RECORDING, 1, 3), recording(RECORDING, 1, 4)];
    let bad = MirrorSnapshot {
        snapshot_sha256: mirror_snapshot_digest(RELEASE, &recordings),
        release_mbid: RELEASE.into(),
        recordings,
    };
    assert_eq!(
        derive_evidence(&hinted(), |_| Some(bad)),
        Err(EvidenceError::Snapshot)
    );
}

#[test]
fn release_mismatch_refuses() {
    let other_release = "eeeeeeee-5555-4555-8555-555555555555";
    let recordings = vec![recording(RECORDING, 1, 3)];
    let bad = MirrorSnapshot {
        snapshot_sha256: mirror_snapshot_digest(other_release, &recordings),
        release_mbid: other_release.into(),
        recordings,
    };
    assert_eq!(
        derive_evidence(&hinted(), |_| Some(bad)),
        Err(EvidenceError::Snapshot)
    );
}

#[test]
fn derived_slots_align_with_entries() {
    let manifest = manifest_with(vec![
        json!({"title": "t", "release_mbid": RELEASE, "recording_mbid": RECORDING}),
        json!({"title": "u"}),
    ]);
    let slots = derive_evidence(&manifest, |_| Some(snapshot())).unwrap();
    assert_eq!(slots.len(), 2);
    assert!(slots[0].is_some());
    assert_eq!(slots[1], None);
}
