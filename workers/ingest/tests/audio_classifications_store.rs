//! Durable classification store seam: serde round trip, binding refusal and
//! pinned SQL text. Database behaviour is covered by the disposable-PostgreSQL
//! API regression; no client is constructed here.
use nntune_ingest::{
    audio_classifications::{classify_entries, ClassificationCheckpoint, MirrorEvidence},
    audio_classifications_store::{StoreError, FREEZE_SQL, LOAD_SQL},
    audio_grouping::prepare_manifest,
    audio_manifest::{AcceptedAudioManifest, AudioHints},
    audio_selection::{AudioSelectionPlan, ExtractedAudio},
    dispatch::content_sha256_hex,
};

const JOB: &str = "class_job";
const OWNER: &str = "owner";
const RECORDING: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const RELEASE: &str = "cccccccc-3333-4333-8333-333333333333";
const SNAPSHOT: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn manifest() -> AcceptedAudioManifest {
    let xml = "<nzb xmlns=\"http://www.newzbin.com/DTD/2003/nzb\"><file poster=\"test\" date=\"0\" subject=\"&quot;0.flac&quot; yEnc (1/1)\"><groups><group>alt.binaries.test</group></groups><segments><segment bytes=\"100\" number=\"1\">part0@example.invalid</segment></segments></file></nzb>";
    let plan = AudioSelectionPlan::from_original(xml, &content_sha256_hex(xml.as_bytes())).unwrap();
    let extracted = plan
        .selections()
        .iter()
        .map(|s| ExtractedAudio {
            audio_ordinal: s.audio_ordinal(),
            selector: s.selector().clone(),
            hints: AudioHints {
                album: Some("Album".into()),
                track_artist: Some("Artist".into()),
                title: Some("A".into()),
                track_no: Some(1),
                ..AudioHints::default()
            },
        })
        .collect();
    prepare_manifest(JOB, OWNER, &plan, extracted).unwrap()
}

fn checkpoints() -> Vec<ClassificationCheckpoint> {
    let frozen = manifest();
    let verified = Some(MirrorEvidence {
        recording_mbid: RECORDING.into(),
        release_mbid: RELEASE.into(),
        snapshot_sha256: SNAPSHOT.into(),
        medium_no: 1,
        track_no: 1,
    });
    vec![
        classify_entries(&frozen, &[None]).unwrap(),
        classify_entries(&frozen, &[verified]).unwrap(),
    ]
}

#[test]
fn checkpoints_round_trip_through_json_exactly() {
    for checkpoint in checkpoints() {
        let raw = serde_json::to_string(&checkpoint).unwrap();
        let parsed = ClassificationCheckpoint::parse(&raw).unwrap();
        assert!(parsed == checkpoint);
        assert_eq!(serde_json::to_string(&parsed).unwrap(), raw);
        assert_eq!(parsed.entries(), checkpoint.entries());
    }
}

#[test]
fn accessors_expose_the_bound_identity_in_valid_shapes() {
    let checkpoint = &checkpoints()[0];
    let hash = content_sha256_hex(b"x");
    assert_eq!(checkpoint.job_id(), JOB);
    assert_eq!(checkpoint.user_id(), OWNER);
    let original = checkpoint.original_content_sha256();
    assert_eq!(original.len(), 64);
    assert!(original
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    assert_eq!(hash.len(), 64);
}

#[test]
fn binding_refuses_wrong_job_or_owner() {
    let checkpoint = &checkpoints()[0];
    assert!(checkpoint.is_for(JOB, OWNER));
    assert!(!checkpoint.is_for("other_job", OWNER));
    assert!(!checkpoint.is_for(JOB, "other-owner"));
    assert!(!checkpoint.is_for(OWNER, JOB));
    assert!(!checkpoint.is_for("", ""));
}

#[test]
fn binding_refuses_a_different_original_hash() {
    let checkpoint = &checkpoints()[0];
    let other = content_sha256_hex(b"different original");
    assert_ne!(checkpoint.original_content_sha256(), other);
    assert_eq!(
        checkpoint.original_content_sha256(),
        manifest().original_content_sha256()
    );
}

#[test]
fn invalid_json_is_refused_on_parse() {
    let valid = serde_json::to_string(&checkpoints()[0]).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&valid).unwrap();
    value["unexpected"] = serde_json::json!(true);
    let mut missing: serde_json::Value = serde_json::from_str(&valid).unwrap();
    missing.as_object_mut().unwrap().remove("entries");
    for raw in [
        "",
        "not json",
        "[]",
        "null",
        "\"text\"",
        "{}",
        &value.to_string(),
        &missing.to_string(),
        &valid[..valid.len() - 1],
    ] {
        assert!(ClassificationCheckpoint::parse(raw).is_err(), "{raw}");
    }
}

#[test]
fn store_errors_are_distinct_copyable_variants() {
    let all = [
        StoreError::Identity,
        StoreError::MissingOrFenced,
        StoreError::InvalidStored,
        StoreError::Permanent,
        StoreError::Unavailable,
    ];
    for (i, a) in all.iter().enumerate() {
        for (j, b) in all.iter().enumerate() {
            assert_eq!(a == b, i == j);
        }
    }
}

#[test]
fn freeze_sql_pins_first_wins_owner_fence_and_returning() {
    let flat = FREEZE_SQL.split_whitespace().collect::<String>();
    assert!(flat.contains("coalesce(original.accepted_audio_classifications_v1,$3)"));
    assert!(FREEZE_SQL.contains("SET accepted_audio_classifications_v1"));
    assert!(FREEZE_SQL.contains("deleting_at IS NULL FOR SHARE"));
    assert!(FREEZE_SQL.contains("job.user_id = $2"));
    assert!(FREEZE_SQL.contains("original.content_sha256 = $4"));
    assert!(FREEZE_SQL.contains("job.content_sha256 = original.content_sha256"));
    assert!(FREEZE_SQL.contains("RETURNING original.accepted_audio_classifications_v1"));
    assert!(!FREEZE_SQL.contains("accepted_audio_manifest_v1"));
}

#[test]
fn load_sql_reads_only_the_classification_column_with_the_same_fences() {
    assert!(LOAD_SQL.contains("accepted_audio_classifications_v1"));
    assert!(!LOAD_SQL.contains("accepted_audio_manifest_v1"));
    assert!(LOAD_SQL.contains("owner.deleting_at IS NULL"));
    assert!(LOAD_SQL.contains("original.content_sha256 = $3"));
    assert!(LOAD_SQL.contains("job.user_id = $2"));
    assert!(!LOAD_SQL.to_ascii_uppercase().contains("UPDATE"));
}
