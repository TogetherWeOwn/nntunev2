//! Contract preparation only: no tag reader, mirror lookup or normalized writer.
use nntune_ingest::{
    audio_manifest::{AcceptedAudioManifest, ManifestError, MAX_MANIFEST_BYTES},
    checkpoint,
    dispatch::build_search_dispatch,
    jobs::{JobCheckpoint, JobState},
    retry::redrive_checkpoint,
};
use serde_json::{json, Value};

fn input() -> Value {
    json!({
        "version": 1, "job_id": "job_manifest", "user_id": "user_manifest",
        "original_content_sha256": "a".repeat(64),
        "entries": [
            {"audio_ordinal": 1, "source_selector": {
                "nzb_file_ordinal": 2, "archive_member": null, "audio_selection_index": 0
            }, "artist_group_ref": 1, "release_group_ref": 1, "hints": {
                "title": "First", "album": "An album", "track_artist": "An artist",
                "track_no": 1
            }},
            {"audio_ordinal": 2, "source_selector": {
                "nzb_file_ordinal": 4, "archive_member": "disc/track.flac", "audio_selection_index": 0
            }, "artist_group_ref": 1, "release_group_ref": 1, "hints": {
                "title": "Second", "track_no": 2
            }}
        ], "art_blobs": []
    })
}

fn parse(value: &Value) -> Result<AcceptedAudioManifest, ManifestError> {
    AcceptedAudioManifest::parse(&value.to_string())
}

#[test]
fn manifest_roundtrip_preserves_selection_and_untrusted_hints() {
    let manifest = parse(&input()).unwrap();
    assert_eq!(manifest.entries()[0].audio_ordinal, 1);
    assert_eq!(manifest.entries()[0].source_selector.nzb_file_ordinal, 2);
    assert_eq!(
        manifest.entries()[1]
            .source_selector
            .archive_member
            .as_deref(),
        Some("disc/track.flac")
    );
    assert_eq!(manifest.entries()[0].hints.track_no, Some(1));
    assert_eq!(manifest.entries()[0].hints.disc_no, None);
    let back = AcceptedAudioManifest::parse(&serde_json::to_string(&manifest).unwrap()).unwrap();
    assert_eq!(manifest, back);
    let debug = format!("{manifest:?}");
    for private in ["disc/track.flac", "First", "job_manifest", "user_manifest"] {
        assert!(!debug.contains(private));
    }
}

#[test]
fn legacy_manifests_do_not_silently_acquire_effective_discs() {
    let manifest = parse(&input()).unwrap();
    assert_eq!(manifest.effective_discs(), None);
    let stored = serde_json::to_value(&manifest).unwrap();
    assert!(stored.get("effective_discs").is_none());
    assert_eq!(parse(&stored).unwrap(), manifest);
}

#[test]
fn frozen_disc_vector_is_complete_positive_and_consistent_with_all_hints() {
    let mut value = input();
    value["effective_discs"] = json!([1, 1]);
    assert_eq!(
        parse(&value).unwrap().effective_discs(),
        Some(&[Some(1); 2][..])
    );
    for bad in [
        json!([]),
        json!([1]),
        json!([1, 1, 1]),
        json!([0, 1]),
        json!([-1, 1]),
        json!([null, 1]),
        json!([2, 1]),
        json!([2147483648_u64, 1]),
    ] {
        value["effective_discs"] = bad;
        assert!(parse(&value).is_err());
    }
    value["effective_discs"] = json!([1, 1]);
    value["entries"][1]["hints"]["disc_no"] = json!(2);
    assert!(
        parse(&value).is_err(),
        "later tag invalidates implicit disc on earlier entry"
    );
    value["effective_discs"] = json!([null, 2]);
    assert!(parse(&value).is_ok());
    value["entries"][1]["hints"]["disc_no"] = json!(0);
    value["effective_discs"] = json!([null, null]);
    assert!(
        parse(&value).is_ok(),
        "present malformed tag is not implicit disc one"
    );
    value["entries"][1]["release_group_ref"] = json!(2);
    assert!(
        parse(&value).is_err(),
        "separate first release has no disc tags"
    );
    value["effective_discs"] = json!([1, null]);
    assert!(parse(&value).is_ok());
}

#[test]
fn disc_vector_duplicate_field_is_rejected_and_not_leaked_by_debug() {
    let mut value = input();
    value["effective_discs"] = json!([1, 1]);
    let manifest = parse(&value).unwrap();
    assert!(!format!("{manifest:?}").contains("effective_discs"));
    let raw = value.to_string().replacen(
        "\"effective_discs\":[1,1]",
        "\"effective_discs\":[1,1],\"effective_discs\":[1,1]",
        1,
    );
    assert!(AcceptedAudioManifest::parse(&raw).is_err());
}

#[test]
fn manifest_rejects_unknown_versions_fields_duplicate_keys_and_oversize() {
    let mut value = input();
    value["version"] = json!(2);
    assert_eq!(parse(&value).unwrap_err(), ManifestError::Invalid);
    value = input();
    value["entries"][0]["canonical_mbid"] = json!("not authority");
    assert_eq!(parse(&value).unwrap_err(), ManifestError::Invalid);
    let text = input()
        .to_string()
        .replacen("\"version\":1", "\"version\":1,\"version\":1", 1);
    assert_eq!(
        AcceptedAudioManifest::parse(&text).unwrap_err(),
        ManifestError::Invalid
    );
    assert_eq!(
        AcceptedAudioManifest::parse(&" ".repeat(MAX_MANIFEST_BYTES + 1)).unwrap_err(),
        ManifestError::TooLarge
    );
}

#[test]
fn audio_ordinals_are_contiguous_and_selection_order_is_total() {
    for (field, bad) in [
        ("audio_ordinal", json!(0)),
        ("audio_ordinal", json!(3)),
        ("release_group_ref", json!(2)),
        ("artist_group_ref", json!(0)),
    ] {
        let mut value = input();
        value["entries"][0][field] = bad;
        assert!(parse(&value).is_err(), "{field}");
    }
    let mut value = input();
    value["entries"][1]["source_selector"] = value["entries"][0]["source_selector"].clone();
    assert!(parse(&value).is_err(), "duplicate selection");
    value["entries"][1]["source_selector"]["nzb_file_ordinal"] = json!(1);
    assert!(parse(&value).is_err(), "unordered selection");
    value = input();
    value["entries"][1]["source_selector"]["archive_member"] = json!("../track.flac");
    assert!(parse(&value).is_err(), "unsafe archive path");
}

#[test]
fn release_groups_cannot_switch_artist_or_refer_to_a_different_group() {
    let mut value = input();
    value["entries"][1]["artist_group_ref"] = json!(2);
    assert!(parse(&value).is_err());
    value = input();
    value["entries"][1]["release_group_ref"] = json!(2);
    assert!(parse(&value).is_ok(), "one artist can span releases");
    value["entries"][1]["artist_group_ref"] = json!(2);
    assert!(parse(&value).is_ok(), "independent artist/release groups");
}

#[test]
fn embedded_art_checkpoints_metadata_once_not_bytes_or_urls() {
    let mut value = input();
    let hash = "b".repeat(64);
    value["art_blobs"] = json!([{"content_hash": hash, "byte_size": 123, "mime": "image/png"}]);
    for entry in value["entries"].as_array_mut().unwrap() {
        entry["embedded_art_selector"] = json!({"tag_container": "flac", "picture_ordinal": 0,
            "declared_mime": "image/png", "content_hash": hash});
    }
    let manifest = parse(&value).unwrap();
    assert_eq!(manifest.art_blobs().len(), 1);
    value["art_blobs"][0]["byte_size"] = json!(5 * 1024 * 1024 + 1);
    assert!(parse(&value).is_err());
    value["art_blobs"][0]["byte_size"] = json!(123);
    value["art_blobs"][0]["mime"] = json!("image/svg+xml");
    assert!(parse(&value).is_err());
    value["art_blobs"][0]["mime"] = json!("image/png");
    let blob = value["art_blobs"][0].clone();
    value["art_blobs"].as_array_mut().unwrap().push(blob);
    assert!(parse(&value).is_err(), "hash checkpoints once");
    value["art_blobs"] = json!([]);
    assert!(parse(&value).is_err(), "missing staged descriptor");
}

#[test]
fn checkpoint_freeze_binds_identity_and_survives_redrive_without_regeneration() {
    let msg = build_search_dispatch("user_manifest", "query", "indexer", &[3000], 25, 0).unwrap();
    let mut cp = checkpoint::initial(&msg);
    cp.state = JobState::Matching;
    let mut value = input();
    value["job_id"] = json!(cp.job_id);
    let manifest = parse(&value).unwrap();
    assert!(cp.freeze_audio_manifest(manifest.clone()).is_ok());
    assert!(cp.freeze_audio_manifest(manifest.clone()).is_ok());
    let before = cp.clone();
    value["entries"][0]["hints"]["title"] = json!("changed tags");
    assert_eq!(
        cp.freeze_audio_manifest(parse(&value).unwrap()),
        Err(ManifestError::Frozen)
    );
    assert_eq!(cp, before);
    redrive_checkpoint(&mut cp);
    assert_eq!(cp.audio_manifest(), Some(&manifest));
    let back: JobCheckpoint = serde_json::from_str(&serde_json::to_string(&cp).unwrap()).unwrap();
    assert_eq!(back.audio_manifest(), Some(&manifest));
    assert_eq!(checkpoint::retention_ttl_secs(&back), None);
    let status = cp.public_status();
    assert!(status.get("accepted_audio_manifest_v1").is_none());
    assert!(!status.to_string().contains("disc/track.flac"));
    assert!(cp.validate_audio_manifest().is_ok());
    cp.user_id = "different_user".into();
    assert_eq!(cp.validate_audio_manifest(), Err(ManifestError::Identity));
    let mut other = checkpoint::initial(&msg);
    other.job_id = "other_job".into();
    assert_eq!(
        other.freeze_audio_manifest(manifest),
        Err(ManifestError::Identity)
    );
    assert!(other.audio_manifest().is_none());
}

#[test]
fn pre_manifest_checkpoints_remain_readable_and_cannot_imply_a_manifest() {
    let msg = build_search_dispatch("user_manifest", "query", "indexer", &[3000], 25, 0).unwrap();
    let cp = checkpoint::initial(&msg);
    let mut value = serde_json::to_value(&cp).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .remove("accepted_audio_manifest_v1");
    let back: JobCheckpoint = serde_json::from_value(value).unwrap();
    assert!(back.audio_manifest().is_none());
    assert_eq!(
        checkpoint::retention_ttl_secs(&back),
        Some(checkpoint::CHECKPOINT_TTL_SECS)
    );
    assert_eq!(back.public_status(), serde_json::to_value(&cp).unwrap());
    assert_eq!(cp, back);
}
