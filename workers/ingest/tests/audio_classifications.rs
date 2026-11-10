//! Classification checkpoint preparation only; no writer or publication.
use nntune_ingest::{
    audio_classifications::{
        classify_entries, ClassError, ClassKind, ClassifiedEntry, MirrorEvidence,
    },
    audio_grouping::prepare_manifest,
    audio_manifest::{AcceptedAudioManifest, AudioHints},
    audio_publication::prepare_publication,
    audio_selection::{AudioSelectionPlan, ExtractedAudio},
    dispatch::content_sha256_hex,
};

const REC_A: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const REC_B: &str = "bbbbbbbb-2222-4222-8222-222222222222";
const RELEASE: &str = "cccccccc-3333-4333-8333-333333333333";
const OTHER_RELEASE: &str = "dddddddd-4444-4444-8444-444444444444";
const SNAPSHOT: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn manifest(hints: Vec<AudioHints>) -> AcceptedAudioManifest {
    let mut xml = String::from("<nzb xmlns=\"http://www.newzbin.com/DTD/2003/nzb\">");
    for i in 0..hints.len() {
        xml.push_str(&format!("<file poster=\"test\" date=\"0\" subject=\"&quot;{i}.flac&quot; yEnc (1/1)\"><groups><group>alt.binaries.test</group></groups><segments><segment bytes=\"100\" number=\"1\">part{i}@example.invalid</segment></segments></file>"));
    }
    xml.push_str("</nzb>");
    let plan =
        AudioSelectionPlan::from_original(&xml, &content_sha256_hex(xml.as_bytes())).unwrap();
    let extracted = plan
        .selections()
        .iter()
        .zip(hints)
        .map(|(s, hints)| ExtractedAudio {
            audio_ordinal: s.audio_ordinal(),
            selector: s.selector().clone(),
            hints,
        })
        .collect();
    prepare_manifest("class_job", "owner", &plan, extracted).unwrap()
}

fn hint(track: Option<i32>, title: &str, duration_ms: Option<u64>) -> AudioHints {
    AudioHints {
        album: Some("Album".into()),
        track_artist: Some("Artist".into()),
        title: Some(title.into()),
        track_no: track,
        duration_ms,
        ..AudioHints::default()
    }
}

fn evidence(recording: &str, medium_no: i32, track_no: i32) -> Option<MirrorEvidence> {
    Some(MirrorEvidence {
        recording_mbid: recording.into(),
        release_mbid: RELEASE.into(),
        snapshot_sha256: SNAPSHOT.into(),
        medium_no,
        track_no,
    })
}

fn two_entries() -> AcceptedAudioManifest {
    manifest(vec![hint(Some(1), "A", None), hint(Some(2), "B", None)])
}

fn entry(
    ordinal: u32,
    kind: ClassKind,
    medium_no: i32,
    track_no: i32,
    duplicate_of: Option<u32>,
) -> ClassifiedEntry {
    ClassifiedEntry {
        audio_ordinal: ordinal,
        release_group_ref: 1,
        kind,
        medium_no,
        track_no,
        duplicate_of,
    }
}

#[test]
fn all_local_manifest_uses_planner_positions_and_keeps_duplicates() {
    let frozen = manifest(vec![
        hint(Some(7), "A", Some(100_000)),
        hint(Some(7), "a", Some(100_000)),
        hint(Some(8), "B", Some(100_000)),
    ]);
    let checkpoint = classify_entries(&frozen, &[None, None, None]).unwrap();
    assert_eq!(
        checkpoint.entries(),
        &[
            entry(1, ClassKind::AllLocal, 1, 7, None),
            entry(2, ClassKind::AllLocal, 1, 7, Some(1)),
            entry(3, ClassKind::AllLocal, 1, 8, None),
        ]
    );
}

#[test]
fn fully_verified_release_takes_positions_from_evidence_and_dedups() {
    let frozen = manifest(vec![
        hint(Some(1), "A", None),
        hint(Some(2), "B", None),
        hint(Some(3), "C", None),
    ]);
    let checkpoint = classify_entries(
        &frozen,
        &[
            evidence(REC_A, 2, 5),
            evidence(REC_A, 2, 5),
            evidence(REC_B, 2, 6),
        ],
    )
    .unwrap();
    assert_eq!(
        checkpoint.entries(),
        &[
            entry(1, ClassKind::Verified, 2, 5, None),
            entry(2, ClassKind::Verified, 2, 5, Some(1)),
            entry(3, ClassKind::Verified, 2, 6, None),
        ]
    );
}

#[test]
fn same_recording_same_position_in_two_release_groups_both_survive() {
    let mut hints = vec![hint(Some(1), "A", None), hint(Some(1), "A", None)];
    hints[1].album = Some("Other".into());
    let frozen = manifest(hints);
    let checkpoint =
        classify_entries(&frozen, &[evidence(REC_A, 1, 1), evidence(REC_A, 1, 1)]).unwrap();
    let rows = checkpoint.entries();
    assert_eq!((rows[0].release_group_ref, rows[0].duplicate_of), (1, None));
    assert_eq!((rows[1].release_group_ref, rows[1].duplicate_of), (2, None));
    assert!(prepare_publication(&frozen, &checkpoint).is_ok());
}

#[test]
fn same_recording_at_different_positions_is_not_a_duplicate() {
    let checkpoint = classify_entries(
        &two_entries(),
        &[evidence(REC_A, 1, 1), evidence(REC_A, 1, 2)],
    )
    .unwrap();
    assert_eq!(checkpoint.entries()[1].duplicate_of, None);
}

#[test]
fn verified_and_unverified_entries_in_one_release_group_are_refused() {
    for slots in [[evidence(REC_A, 1, 1), None], [None, evidence(REC_B, 1, 2)]] {
        assert_eq!(
            classify_entries(&two_entries(), &slots).unwrap_err(),
            ClassError::MixedRelease
        );
    }
}

#[test]
fn evidence_release_contradicting_the_entry_hint_is_refused() {
    let mut hints = vec![hint(Some(1), "A", None), hint(Some(2), "B", None)];
    for h in &mut hints {
        h.release_mbid = Some(OTHER_RELEASE.into());
    }
    assert_eq!(
        classify_entries(
            &manifest(hints.clone()),
            &[evidence(REC_A, 1, 1), evidence(REC_B, 1, 2)]
        )
        .unwrap_err(),
        ClassError::Evidence
    );
    // A matching hint (compared case-insensitively) is accepted.
    for h in &mut hints {
        h.release_mbid = Some(RELEASE.to_ascii_uppercase());
    }
    assert!(classify_entries(
        &manifest(hints),
        &[evidence(REC_A, 1, 1), evidence(REC_B, 1, 2)]
    )
    .is_ok());
}

#[test]
fn malformed_snapshot_digest_is_refused() {
    let upper = SNAPSHOT.to_ascii_uppercase();
    let bad = [
        "",
        "abc123",
        &SNAPSHOT[..63],
        &format!("{SNAPSHOT}0"),
        &SNAPSHOT.replace('0', "g"),
        upper.as_str(),
    ];
    for digest in bad {
        let mut ev = evidence(REC_A, 1, 1);
        ev.as_mut().unwrap().snapshot_sha256 = digest.into();
        assert_eq!(
            classify_entries(&two_entries(), &[ev, evidence(REC_B, 1, 2)]).unwrap_err(),
            ClassError::Evidence,
            "digest {digest:?}"
        );
    }
}

#[test]
fn evidence_slice_length_must_match_entries() {
    for slots in [
        vec![],
        vec![evidence(REC_A, 1, 1)],
        vec![evidence(REC_A, 1, 1), evidence(REC_B, 1, 2), None],
    ] {
        assert_eq!(
            classify_entries(&two_entries(), &slots).unwrap_err(),
            ClassError::Evidence
        );
    }
}

#[test]
fn non_positive_evidence_positions_are_refused() {
    for (medium, track) in [(0, 1), (-1, 1), (1, 0), (1, -1)] {
        assert_eq!(
            classify_entries(
                &two_entries(),
                &[evidence(REC_A, medium, track), evidence(REC_B, 1, 2)]
            )
            .unwrap_err(),
            ClassError::Evidence,
            "({medium}, {track})"
        );
    }
}

#[test]
fn malformed_evidence_mbids_are_refused() {
    for (recording, release) in [("not-an-mbid", RELEASE), (REC_A, "short")] {
        let mut ev = evidence(recording, 1, 1);
        ev.as_mut().unwrap().release_mbid = release.into();
        assert_eq!(
            classify_entries(&two_entries(), &[ev, evidence(REC_B, 1, 2)]).unwrap_err(),
            ClassError::Evidence
        );
    }
}

#[test]
fn recording_mbid_dedup_is_case_insensitive() {
    let checkpoint = classify_entries(
        &two_entries(),
        &[
            evidence(REC_A, 1, 3),
            evidence(&REC_A.to_ascii_uppercase(), 1, 3),
        ],
    )
    .unwrap();
    assert_eq!(
        checkpoint.entries(),
        &[
            entry(1, ClassKind::Verified, 1, 3, None),
            entry(2, ClassKind::Verified, 1, 3, Some(1)),
        ]
    );
}

#[test]
fn checkpoint_entries_carry_ordinals_and_groups_and_hide_bindings() {
    let mut hints = vec![hint(Some(1), "A", None), hint(Some(2), "B", None)];
    hints[1].album = Some("Other".into());
    let checkpoint = classify_entries(&manifest(hints), &[evidence(REC_A, 1, 1), None]).unwrap();
    let rows = checkpoint.entries();
    assert_eq!(rows.len(), 2);
    assert_eq!((rows[0].audio_ordinal, rows[0].release_group_ref), (1, 1));
    assert_eq!((rows[1].audio_ordinal, rows[1].release_group_ref), (2, 2));
    assert_eq!(rows[0].kind, ClassKind::Verified);
    assert_eq!(rows[1].kind, ClassKind::AllLocal);
    // job_id, user_id and original hash are crate-private; only the
    // redacted Debug form is observable here.
    assert_eq!(
        format!("{checkpoint:?}"),
        "ClassificationCheckpoint([redacted])"
    );
}

#[test]
fn legacy_unfrozen_manifest_is_refused() {
    let frozen = two_entries();
    let mut raw = serde_json::to_value(&frozen).unwrap();
    raw.as_object_mut().unwrap().remove("effective_discs");
    let legacy = AcceptedAudioManifest::parse(&raw.to_string()).unwrap();
    assert!(matches!(
        classify_entries(&legacy, &[None, None]).unwrap_err(),
        ClassError::Positions(_)
    ));
}
