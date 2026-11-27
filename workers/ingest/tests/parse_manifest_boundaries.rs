use nntune_ingest::{
    audio_classifications::{
        classify_entries, ClassError, ClassificationCheckpoint, MirrorEvidence,
    },
    audio_grouping::{prepare_manifest, GroupingError},
    audio_manifest::{AcceptedAudioManifest, AudioHints, ManifestError},
    audio_selection::{AudioSelectionPlan, ExtractedAudio},
    audio_tags::{read_flac_hints, TagReadError},
    dispatch::content_sha256_hex,
    fixtures::SMALL_NZB,
    nzb_intake::{estimate_nzb, DEFAULT_LIMITS},
    parse::{check_parsed, parse_upload, parse_upload_files, ParseRejection},
};
use serde_json::json;

#[test]
fn nzb_projections_refuse_disagreeing_declared_bytes() {
    let estimate = estimate_nzb(SMALL_NZB).unwrap();
    let parsed = parse_upload(SMALL_NZB.as_bytes(), &estimate).unwrap();
    assert!(parse_upload_files(SMALL_NZB.as_bytes(), &estimate).is_ok());
    for declared_bytes in [199, 201] {
        let mut mismatched = estimate;
        mismatched.declared_bytes = declared_bytes;
        assert!(matches!(
            parse_upload(SMALL_NZB.as_bytes(), &mismatched),
            Err(ParseRejection::Invalid(_))
        ));
        assert!(matches!(
            parse_upload_files(SMALL_NZB.as_bytes(), &mismatched),
            Err(ParseRejection::Invalid(_))
        ));
        assert!(check_parsed(&mismatched, &parsed).is_err());
    }
}

#[test]
fn nzb_projections_enforce_the_intake_document_cap() {
    let mut xml = SMALL_NZB.to_owned();
    xml.extend(std::iter::repeat_n(
        ' ',
        DEFAULT_LIMITS.max_nzb_bytes - xml.len(),
    ));
    let estimate = estimate_nzb(&xml).unwrap();
    assert!(parse_upload(xml.as_bytes(), &estimate).is_ok());
    assert!(parse_upload_files(xml.as_bytes(), &estimate).is_ok());
    xml.push(' ');
    assert_eq!(
        parse_upload(xml.as_bytes(), &estimate),
        Err(ParseRejection::TooLarge)
    );
    assert_eq!(
        parse_upload_files(xml.as_bytes(), &estimate),
        Err(ParseRejection::TooLarge)
    );
}

#[test]
fn flac_refuses_invalid_comment_vendor_encoding() {
    let mut prefix = b"fLaC\0\0\0\x22".to_vec();
    let mut info = [0; 34];
    info[10..18].copy_from_slice(&((44_100_u64 << 44) | 88_200).to_be_bytes());
    prefix.extend(info);
    prefix.extend_from_slice(b"\x84\0\0\x09\x01\0\0\0x\0\0\0\0");
    assert_eq!(read_flac_hints(&prefix).unwrap().duration_ms, Some(2000));
    prefix[50] = 0xff;
    assert_eq!(
        read_flac_hints(&prefix).err(),
        Some(TagReadError::Malformed)
    );
}

fn plan_and_hints() -> (AudioSelectionPlan, Vec<ExtractedAudio>) {
    let xml = SMALL_NZB.replace(
        "</nzb>",
        r#"<file date="1" subject="track02.flac"><segment bytes="100" number="1">seg-three@synthetic.invalid</segment></file></nzb>"#,
    );
    let plan =
        AudioSelectionPlan::from_original(&xml, &content_sha256_hex(xml.as_bytes())).unwrap();
    let extracted = plan
        .selections()
        .iter()
        .map(|selection| ExtractedAudio {
            audio_ordinal: selection.audio_ordinal(),
            selector: selection.selector().clone(),
            hints: AudioHints {
                album: Some("Test album".into()),
                track_artist: Some("Test artist".into()),
                ..AudioHints::default()
            },
        })
        .collect();
    (plan, extracted)
}

#[test]
fn manifest_storage_and_grouping_share_the_ingest_owner_boundary() {
    let (plan, extracted) = plan_and_hints();
    assert_eq!(
        prepare_manifest("job_fixture", "owner:other", &plan, extracted),
        Err(GroupingError::Manifest(ManifestError::Invalid))
    );
    let mut stored = serde_json::to_value(manifest()).unwrap();
    stored["user_id"] = json!("owner:other");
    assert_eq!(
        AcceptedAudioManifest::parse(&stored.to_string()),
        Err(ManifestError::Invalid)
    );
    assert!(serde_json::from_value::<AcceptedAudioManifest>(stored).is_err());
}

fn manifest() -> AcceptedAudioManifest {
    let (plan, extracted) = plan_and_hints();
    prepare_manifest("job_fixture", "owner_fixture", &plan, extracted).unwrap()
}

fn evidence(track_no: i32) -> MirrorEvidence {
    MirrorEvidence {
        recording_mbid: "aaaaaaaa-1111-4111-8111-111111111111".into(),
        release_mbid: "bbbbbbbb-2222-4222-8222-222222222222".into(),
        snapshot_sha256: "a".repeat(64),
        medium_no: 1,
        track_no,
    }
}

#[test]
fn verified_group_refuses_conflicting_release_snapshot_or_position() {
    let manifest = manifest();
    let first = evidence(1);
    let mut same = first.clone();
    same.release_mbid.make_ascii_uppercase();
    let valid = classify_entries(&manifest, &[Some(first.clone()), Some(same)]).unwrap();
    assert_eq!(valid.entries()[1].duplicate_of, Some(1));

    let mut other_release = evidence(2);
    other_release.release_mbid = "cccccccc-3333-4333-8333-333333333333".into();
    let mut other_snapshot = evidence(2);
    other_snapshot.snapshot_sha256 = "b".repeat(64);
    let mut other_recording = evidence(1);
    other_recording.recording_mbid = "dddddddd-4444-4444-8444-444444444444".into();
    for second in [other_release, other_snapshot, other_recording] {
        assert_eq!(
            classify_entries(&manifest, &[Some(first.clone()), Some(second)]),
            Err(ClassError::Evidence)
        );
    }
}

#[test]
fn classification_readback_refuses_unknown_version_and_mixed_kind() {
    let checkpoint = classify_entries(&manifest(), &[None, None]).unwrap();
    let valid = serde_json::to_value(&checkpoint).unwrap();
    assert_eq!(
        ClassificationCheckpoint::parse(&valid.to_string()).unwrap(),
        checkpoint
    );
    let mut unknown = valid.clone();
    unknown["version"] = json!(2);
    let mut mixed = valid;
    mixed["entries"][1]["kind"] = json!("verified");
    for value in [unknown, mixed] {
        assert_eq!(
            ClassificationCheckpoint::parse(&value.to_string()),
            Err(ClassError::Evidence)
        );
        assert!(serde_json::from_value::<ClassificationCheckpoint>(value).is_err());
    }
}
