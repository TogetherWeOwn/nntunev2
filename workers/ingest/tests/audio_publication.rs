//! Publication planning: manifest and checkpoint winners join a canonical
//! survivor plan; identity, mismatch, duplicate linkage and legacy geometry
//! each refuse the whole plan.
use nntune_ingest::{
    audio_classifications::{
        classify_entries, ClassKind, ClassificationCheckpoint, MirrorEvidence,
    },
    audio_manifest::AcceptedAudioManifest,
    audio_publication::{prepare_publication, PublicationError},
};
use serde_json::json;

const JOB: &str = "job_0123456789abcdef";
const OWNER: &str = "owner-1";
const RECORDING: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const RELEASE: &str = "cccccccc-3333-4333-8333-333333333333";
const SNAPSHOT: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

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

fn manifest(titles: &[&str]) -> AcceptedAudioManifest {
    manifest_with(
        titles
            .iter()
            .map(|t| json!({"title": t, "track_no": 1}))
            .collect(),
    )
}

fn verified(recording: &str, medium_no: i32, track_no: i32) -> Option<MirrorEvidence> {
    Some(MirrorEvidence {
        recording_mbid: recording.into(),
        release_mbid: RELEASE.into(),
        snapshot_sha256: SNAPSHOT.into(),
        medium_no,
        track_no,
    })
}

fn checkpoint(
    manifest: &AcceptedAudioManifest,
    evidence: &[Option<MirrorEvidence>],
) -> ClassificationCheckpoint {
    classify_entries(manifest, evidence).unwrap()
}

fn mutated(
    manifest: &AcceptedAudioManifest,
    evidence: &[Option<MirrorEvidence>],
    mutate: impl FnOnce(&mut serde_json::Value),
) -> ClassificationCheckpoint {
    let mut value = serde_json::to_value(checkpoint(manifest, evidence)).unwrap();
    mutate(&mut value);
    ClassificationCheckpoint::parse(&value.to_string()).unwrap()
}

#[test]
fn survivors_carry_selector_hints_and_classified_positions() {
    let frozen = manifest(&["A", "B"]);
    let frozen_checkpoint = checkpoint(&frozen, &[None, None]);
    let plan = prepare_publication(&frozen, &frozen_checkpoint).unwrap();
    assert_eq!(plan.entries().len(), 2);
    assert_eq!(plan.entries()[0].audio_ordinal, 1);
    assert_eq!(plan.entries()[0].kind, ClassKind::AllLocal);
    assert_eq!(plan.entries()[0].source_selector.nzb_file_ordinal, 0);
    assert_eq!(plan.entries()[1].source_selector.nzb_file_ordinal, 1);
    assert_eq!(plan.entries()[0].hints.title.as_deref(), Some("A"));
    assert_eq!(plan.entries()[0].release_group_ref, 1);
    assert_eq!(plan.entries()[0].artist_group_ref, 1);
    assert_eq!(format!("{plan:?}"), "PublicationPlan([redacted])");
}

#[test]
fn verified_positions_flow_from_checkpoint_not_hints() {
    let frozen = manifest_with(vec![
        json!({"title": "A", "track_no": 1, "release_mbid": RELEASE, "recording_mbid": RECORDING}),
        json!({"title": "B", "track_no": 2, "release_mbid": RELEASE}),
    ]);
    let frozen_checkpoint = checkpoint(
        &frozen,
        &[
            verified(RECORDING, 2, 5),
            verified("dddddddd-4444-4444-8444-444444444444", 2, 6),
        ],
    );
    let plan = prepare_publication(&frozen, &frozen_checkpoint).unwrap();
    assert_eq!(plan.entries().len(), 2);
    assert_eq!(plan.entries()[0].kind, ClassKind::Verified);
    assert_eq!(
        (plan.entries()[0].medium_no, plan.entries()[0].track_no),
        (2, 5)
    );
    assert_eq!(
        (plan.entries()[1].medium_no, plan.entries()[1].track_no),
        (2, 6)
    );
    // Hinted MBIDs ride along as claims alongside the classified positions.
    assert_eq!(
        plan.entries()[0].hints.recording_mbid.as_deref(),
        Some(RECORDING)
    );
    assert_eq!(
        plan.entries()[0].hints.release_mbid.as_deref(),
        Some(RELEASE)
    );
}

#[test]
fn losing_duplicates_write_no_publication_entry() {
    let frozen = manifest_with(vec![
        json!({"title": "A", "track_no": 1, "duration_ms": 100000}),
        json!({"title": "A", "track_no": 1, "duration_ms": 100000}),
        json!({"title": "B", "track_no": 2, "duration_ms": 100000}),
    ]);
    let frozen_checkpoint = checkpoint(&frozen, &[None, None, None]);
    let plan = prepare_publication(&frozen, &frozen_checkpoint).unwrap();
    assert_eq!(
        plan.entries()
            .iter()
            .map(|e| e.audio_ordinal)
            .collect::<Vec<_>>(),
        [1, 3]
    );
    assert!(plan
        .entries()
        .iter()
        .all(|e| e.medium_no > 0 && e.track_no > 0));
}

#[test]
fn checkpoint_binding_must_match_manifest_identity() {
    let frozen = manifest(&["A"]);
    let valid = checkpoint(&frozen, &[None]);
    let mut value = serde_json::to_value(&valid).unwrap();
    for (job, owner, digest) in [
        ("other_job", OWNER, hash()),
        (JOB, "other_owner", hash()),
        (JOB, OWNER, "b".repeat(64)),
    ] {
        let mut copy = value.clone();
        copy["job_id"] = json!(job);
        copy["user_id"] = json!(owner);
        copy["original_content_sha256"] = json!(digest);
        let bad = ClassificationCheckpoint::parse(&copy.to_string()).unwrap();
        assert_eq!(
            prepare_publication(&frozen, &bad),
            Err(PublicationError::Identity),
            "{job} {owner} {digest}"
        );
    }
    let _ = &mut value;
}

#[test]
fn length_ordinal_and_group_mismatches_refuse() {
    let frozen = manifest(&["A", "B"]);
    let short = mutated(&frozen, &[None, None], |v| {
        v["entries"] = json!([v["entries"][0].clone()]);
    });
    assert_eq!(
        prepare_publication(&frozen, &short),
        Err(PublicationError::Mismatch)
    );
    let wrong_ordinal = mutated(&frozen, &[None, None], |v| {
        v["entries"][1]["audio_ordinal"] = json!(3);
    });
    assert_eq!(
        prepare_publication(&frozen, &wrong_ordinal),
        Err(PublicationError::Mismatch)
    );
    let wrong_group = mutated(&frozen, &[None, None], |v| {
        v["entries"][1]["release_group_ref"] = json!(9);
    });
    assert_eq!(
        prepare_publication(&frozen, &wrong_group),
        Err(PublicationError::Mismatch)
    );
}

#[test]
fn legacy_unfrozen_manifest_refuses() {
    let frozen = manifest(&["A"]);
    let mut raw = serde_json::to_value(&frozen).unwrap();
    raw.as_object_mut().unwrap().remove("effective_discs");
    let legacy = AcceptedAudioManifest::parse(&raw.to_string()).unwrap();
    let legacy_checkpoint = checkpoint(
        &manifest_with(vec![json!({"title": "A", "track_no": 1})]),
        &[None],
    );
    // A frozen copy supplies a checkpoint bound to the same identity.
    assert_eq!(
        prepare_publication(&legacy, &legacy_checkpoint),
        Err(PublicationError::Unfrozen)
    );
}

#[test]
fn non_positive_positions_refuse() {
    let frozen = manifest(&["A"]);
    for (medium_no, track_no) in [(0, 1), (-1, 1), (1, 0), (1, -1)] {
        let bad = mutated(&frozen, &[None], |v| {
            v["entries"][0]["medium_no"] = json!(medium_no);
            v["entries"][0]["track_no"] = json!(track_no);
        });
        assert_eq!(
            prepare_publication(&frozen, &bad),
            Err(PublicationError::Positions),
            "({medium_no}, {track_no})"
        );
    }
}

#[test]
fn duplicate_linkage_must_point_at_a_lower_survivor_at_the_same_position() {
    let frozen = manifest(&["A", "B", "C"]);
    // Duplicate points at itself.
    let self_dup = mutated(&frozen, &[None, None, None], |v| {
        v["entries"][1]["duplicate_of"] = json!(2);
    });
    assert_eq!(
        prepare_publication(&frozen, &self_dup),
        Err(PublicationError::Positions)
    );
    // Duplicate points at a higher ordinal.
    let forward = mutated(&frozen, &[None, None, None], |v| {
        v["entries"][0]["duplicate_of"] = json!(2);
    });
    assert_eq!(
        prepare_publication(&frozen, &forward),
        Err(PublicationError::Positions)
    );
    // Duplicate points at a loser (chain).
    let chain = mutated(&frozen, &[None, None, None], |v| {
        v["entries"][1]["duplicate_of"] = json!(1);
        v["entries"][2]["duplicate_of"] = json!(2);
    });
    assert_eq!(
        prepare_publication(&frozen, &chain),
        Err(PublicationError::Positions)
    );
    // Duplicate's position differs from its winner.
    let drift = mutated(&frozen, &[None, None, None], |v| {
        v["entries"][1]["duplicate_of"] = json!(1);
        v["entries"][1]["track_no"] = json!(99);
    });
    assert_eq!(
        prepare_publication(&frozen, &drift),
        Err(PublicationError::Positions)
    );
}

#[test]
fn two_survivors_at_one_release_position_refuse() {
    let frozen = manifest(&["A", "B"]);
    let clash = mutated(&frozen, &[None, None], |v| {
        v["entries"][1]["medium_no"] = v["entries"][0]["medium_no"].clone();
        v["entries"][1]["track_no"] = v["entries"][0]["track_no"].clone();
    });
    assert_eq!(
        prepare_publication(&frozen, &clash),
        Err(PublicationError::Positions)
    );
}
