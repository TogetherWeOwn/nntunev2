//! All-local occurrence preparation only; no mirror identity or writer.
use nntune_ingest::{
    audio_grouping::prepare_manifest,
    audio_manifest::{AcceptedAudioManifest, AudioHints},
    audio_positions::{prepare_all_local_occurrences, LocalOccurrence, PositionError},
    audio_selection::{AudioSelectionPlan, ExtractedAudio},
    dispatch::content_sha256_hex,
};

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
    prepare_manifest("positions_job", "owner", &plan, extracted).unwrap()
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

fn positions(hints: Vec<AudioHints>) -> Vec<LocalOccurrence> {
    prepare_all_local_occurrences(&manifest(hints))
        .unwrap()
        .entries()
        .to_vec()
}

fn survivor(ordinal: u32, disc: i32, track: i32) -> LocalOccurrence {
    LocalOccurrence {
        audio_ordinal: ordinal,
        release_group_ref: 1,
        medium_no: disc,
        track_no: track,
        duplicate_of: None,
    }
}

#[test]
fn album_format_copies_collapse_before_position_fallback() {
    let hints = (0..24)
        .map(|i| hint(Some(i % 12 + 1), &format!("Song {}", i % 12), Some(100_000)))
        .collect();
    let rows = positions(hints);
    assert_eq!(rows.len(), 24);
    for i in 0..12 {
        assert_eq!(rows[i], survivor(i as u32 + 1, 1, i as i32 + 1));
        assert_eq!(rows[i + 12].duplicate_of, Some(i as u32 + 1));
        assert_eq!(
            (rows[i + 12].medium_no, rows[i + 12].track_no),
            (1, i as i32 + 1)
        );
    }
}

#[test]
fn normalized_title_and_symmetric_duration_tolerance_are_required_together() {
    let rows = positions(vec![
        hint(Some(7), " A  Song ", Some(100_000)),
        hint(Some(7), "a song", Some(102_000)),
    ]);
    assert_eq!(rows[0], survivor(1, 1, 7));
    assert_eq!(rows[1].duplicate_of, Some(1));
    for (a, b) in [(1_000_000, 1_010_000), (1_010_000, 1_000_000)] {
        assert_eq!(
            positions(vec![
                hint(Some(1), "A", Some(a)),
                hint(Some(1), "a", Some(b))
            ])[1]
                .duplicate_of,
            Some(1)
        );
    }
    let rows = positions(vec![
        hint(Some(7), "A", Some(100_000)),
        hint(Some(7), "a", Some(102_001)),
    ]);
    assert_eq!(rows, vec![survivor(1, 1, 1), survivor(2, 1, 2)]);
}

#[test]
fn contradictory_pairs_force_whole_release_to_ordinals_without_dropping_files() {
    let rows = positions(vec![
        hint(Some(9), "A", Some(100_000)),
        hint(Some(9), "B", Some(100_000)),
        hint(Some(12), "C", Some(100_000)),
    ]);
    assert_eq!(
        rows,
        vec![survivor(1, 1, 1), survivor(2, 1, 2), survivor(3, 1, 3)]
    );
}

#[test]
fn absent_invalid_or_partial_pairs_force_group_fallback() {
    for missing in [None, Some(0), Some(-1)] {
        let rows = positions(vec![hint(Some(9), "A", None), hint(missing, "B", None)]);
        assert_eq!(rows, vec![survivor(1, 1, 1), survivor(2, 1, 2)]);
    }
    let mut hints = vec![hint(Some(9), "A", None), hint(Some(12), "B", None)];
    hints[0].disc_no = Some(2);
    assert_eq!(positions(hints), vec![survivor(1, 1, 1), survivor(2, 1, 2)]);
}

#[test]
fn distinct_discs_and_tracks_never_collapse_on_identity_alone() {
    let mut hints = vec![hint(Some(7), "A", Some(100_000)); 3];
    hints[0].disc_no = Some(1);
    hints[1].disc_no = Some(2);
    hints[2].disc_no = Some(2);
    hints[2].track_no = Some(8);
    assert_eq!(
        positions(hints),
        vec![survivor(1, 1, 7), survivor(2, 2, 7), survivor(3, 2, 8)]
    );
}

#[test]
fn claimed_recording_identity_is_case_insensitive_but_needs_a_pair() {
    let mut hints = vec![
        hint(Some(7), "A", None),
        hint(Some(7), "B", None),
        hint(None, "C", None),
    ];
    hints[0].recording_mbid = Some("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into());
    hints[1].recording_mbid = Some("AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA".into());
    hints[2].recording_mbid = hints[0].recording_mbid.clone();
    let rows = positions(hints);
    assert_eq!(rows[1].duplicate_of, Some(1));
    assert_eq!(rows[0], survivor(1, 1, 1));
    assert_eq!(rows[2], survivor(3, 1, 3));
    assert_eq!((rows[1].medium_no, rows[1].track_no), (1, 1));
}

#[test]
fn nonempty_fingerprint_agreement_can_identify_local_copies() {
    let mut hints = vec![hint(Some(7), "A", None), hint(Some(7), "B", None)];
    hints[0].fingerprint = Some("measured-fingerprint".into());
    hints[1].fingerprint = hints[0].fingerprint.clone();
    assert_eq!(positions(hints.clone())[1].duplicate_of, Some(1));
    for empty in [None, Some(String::new())] {
        hints[0].fingerprint = empty.clone();
        hints[1].fingerprint = empty;
        assert_eq!(
            positions(hints.clone()),
            vec![survivor(1, 1, 1), survivor(2, 1, 2)]
        );
    }
}

#[test]
fn missing_or_empty_titles_and_durations_do_not_prove_identity() {
    for title in ["", "   ", "A"] {
        assert_eq!(
            positions(vec![hint(Some(7), title, None); 2]),
            vec![survivor(1, 1, 1), survivor(2, 1, 2)]
        );
    }
    assert_eq!(
        positions(vec![hint(Some(7), " ", Some(100_000)); 2]),
        vec![survivor(1, 1, 1), survivor(2, 1, 2)]
    );
}

#[test]
fn duration_tolerance_is_not_transitive_and_losers_never_bridge_identities() {
    let rows = positions(vec![
        hint(Some(7), "A", Some(100_000)),
        hint(Some(7), "A", Some(102_000)),
        hint(Some(7), "A", Some(104_000)),
    ]);
    assert_eq!(rows[0], survivor(1, 1, 1));
    assert_eq!(rows[1].duplicate_of, Some(1));
    assert_eq!(rows[2], survivor(3, 1, 3));
}

#[test]
fn release_groups_remain_independent_even_with_equal_claims() {
    let mut hints = vec![hint(Some(7), "A", Some(100_000)); 2];
    hints[1].album = Some("Other".into());
    let rows = positions(hints);
    assert_eq!(rows[0], survivor(1, 1, 7));
    assert_eq!(
        rows[1],
        LocalOccurrence {
            release_group_ref: 2,
            ..survivor(2, 1, 7)
        }
    );
}

#[test]
fn legacy_manifests_refuse_instead_of_reconstructing_discs() {
    let frozen = manifest(vec![hint(Some(7), "A", None)]);
    let mut raw = serde_json::to_value(&frozen).unwrap();
    raw.as_object_mut().unwrap().remove("effective_discs");
    let legacy = AcceptedAudioManifest::parse(&raw.to_string()).unwrap();
    assert_eq!(
        prepare_all_local_occurrences(&legacy).unwrap_err(),
        PositionError::UnfrozenDiscs
    );
}

#[test]
fn non_ascii_titles_refuse_and_u64_duration_math_cannot_overflow() {
    let frozen = manifest(vec![hint(Some(7), "Élan", Some(100_000))]);
    assert_eq!(
        prepare_all_local_occurrences(&frozen).unwrap_err(),
        PositionError::UnsupportedText
    );
    assert_eq!(
        positions(vec![hint(Some(7), "A", Some(u64::MAX)); 2])[1].duplicate_of,
        Some(1)
    );
    assert_eq!(
        positions(vec![
            hint(Some(7), "A", Some(1)),
            hint(Some(7), "A", Some(u64::MAX))
        ]),
        vec![survivor(1, 1, 1), survivor(2, 1, 2)]
    );
}

#[test]
fn excessive_identity_comparisons_refuse_the_entire_plan() {
    let frozen = manifest(
        (0..500)
            .map(|i| hint(Some(1), &format!("Song {i}"), None))
            .collect(),
    );
    assert_eq!(
        prepare_all_local_occurrences(&frozen).unwrap_err(),
        PositionError::WorkLimit
    );
}

#[test]
fn planning_preserves_manifest_bytes_and_redrive_output() {
    let frozen = manifest(vec![hint(Some(7), "A", Some(100_000)); 2]);
    let before = serde_json::to_string(&frozen).unwrap();
    let first = prepare_all_local_occurrences(&frozen).unwrap();
    let replay = AcceptedAudioManifest::parse(&before).unwrap();
    assert_eq!(first, prepare_all_local_occurrences(&replay).unwrap());
    assert_eq!(serde_json::to_string(&frozen).unwrap(), before);
    assert_eq!(format!("{first:?}"), "LocalOccurrencePlan([redacted])");
}
