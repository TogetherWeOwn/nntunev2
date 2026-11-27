use std::collections::BTreeMap;

use nntune_core::par2::scratch::sha256_hex;
use nntune_ingest::jobs::JobState;
use nntune_ingest::matching::admin_policy::{evaluate_positions, Position, PositionalEligibility};
use nntune_ingest::matching::{
    parse_mb_search_response, pick_release_mbid, MatchFailure, MbSearchHit,
};
use nntune_ingest::parse::ParsedFile;
use nntune_ingest::probe::{
    apply_probe, apply_probe_batch, ProbeOutcome, ProbeTally, MAX_PROBE_SEGMENTS,
};
use nntune_ingest::repair::{classify_repair_reply, decide_repair, RepairCall};
use nntune_ingest::transcode::{
    classify_transcode_reply, decide_transcode, derive_transcode_staging, select_transcode_inputs,
    verify_transcode_artifact, TranscodeCall,
};
use nntune_ingest::verify::{StagedFile, VerifyProgress};

const JOB: &str = "job_fixture";
const USER: &str = "user_fixture";
const KEY: &str = "ingest:user_fixture:abc:upload";

#[test]
fn matching_refuses_invalid_scores_and_the_exact_margin() {
    let body = r#"{"results":[{"mbid":"12345678-0000-0000-0000-000000000000","title":"Album","artist_name":"Artist","score":0.8}]}"#;
    let base = parse_mb_search_response(200, body).unwrap().remove(0);
    let invalid = body.replace("0.8", "1.1");
    assert_eq!(
        parse_mb_search_response(200, &invalid),
        Err(MatchFailure::Transient)
    );
    for score in [f32::NAN, -0.1, 1.1] {
        let malformed = MbSearchHit {
            score,
            ..base.clone()
        };
        let hits = [malformed];
        assert!(pick_release_mbid("Album", &hits).is_none());
        assert!(pick_release_mbid("Artist - Album", &hits).is_none());
    }
    let mut rival = MbSearchHit {
        mbid: "87654321-0000-0000-0000-000000000000".into(),
        score: 0.7,
        ..base.clone()
    };
    assert!(pick_release_mbid("Album", &[rival.clone(), base.clone()]).is_none());
    rival.score = 0.69;
    let expected = base.mbid.clone();
    assert_eq!(pick_release_mbid("Album", &[rival, base]), Some(expected));
}

#[test]
fn positions_at_the_mirror_limit_cannot_prove_completeness() {
    let positions: Vec<_> = (1..=1000)
        .map(|track| Position {
            medium_no: Some(1),
            track_no: Some(track),
        })
        .collect();
    let at_limit = evaluate_positions(1000, &positions, &positions);
    assert_ne!(at_limit, PositionalEligibility::Eligible);
    let below_limit = &positions[..999];
    let eligible = evaluate_positions(999, below_limit, below_limit);
    assert_eq!(eligible, PositionalEligibility::Eligible);
}

#[test]
fn empty_and_corrupt_probe_evidence_cannot_complete() {
    let mut outcome = ProbeOutcome {
        batch_index: 0,
        present: vec![],
        missing: vec![],
    };
    let mut tally = ProbeTally::new(1);
    assert_eq!(
        apply_probe(JobState::Probing, &outcome).0,
        JobState::Probing
    );
    for present in [
        vec![],
        vec!["invalid".into()],
        vec!["a@example.invalid".into(); MAX_PROBE_SEGMENTS + 1],
    ] {
        outcome.present = present;
        let next = apply_probe_batch(JobState::Probing, &mut tally, &outcome);
        assert_eq!(next, (JobState::Probing, 0));
        assert!(tally.missing_by_batch.is_empty());
    }
    outcome.present = vec!["a@example.invalid".into()];
    let next = apply_probe_batch(JobState::Probing, &mut tally, &outcome);
    assert_eq!(next, (JobState::Verifying, 0));
    assert!(!ProbeTally::new(0).is_complete());
    tally.missing_by_batch = BTreeMap::from([(1, 0)]);
    assert!(!tally.is_complete());
    tally.missing_by_batch = BTreeMap::from([(0, MAX_PROBE_SEGMENTS as u32 + 1)]);
    assert!(!tally.is_complete());
    tally.missing_by_batch = BTreeMap::from([(0, u32::MAX), (1, 1)]);
    assert_eq!(tally.missing_segments(), u32::MAX);
}

#[test]
fn repair_dispatch_rejects_fields_the_front_refuses() {
    for (user, key, set) in [("", KEY, "set"), (USER, KEY, "set\0")] {
        assert!(decide_repair(JOB, user, key, set, 1, 1).is_err());
    }
    assert!(decide_repair(JOB, USER, &"k".repeat(513), "set", 1, 1).is_err());
    let accepted = decide_repair(JOB, USER, KEY, "set", 1, 1).unwrap();
    assert!(accepted.is_some());
}

#[test]
fn service_replies_reject_duplicate_decision_fields() {
    let duplicate = br#"{"status":"unrepairable","status":"repaired"}"#;
    assert_eq!(classify_repair_reply(200, duplicate), RepairCall::Retry);
    let repair = br#"{"status":"repaired","file_count":2}"#;
    assert_eq!(classify_repair_reply(200, repair), RepairCall::Repaired);

    let sha = "A".repeat(64);
    let valid = format!(r#"{{"status":"transcoded","artifact_sha256":"{sha}","byte_size":12}}"#);
    for reply in [
        valid.replacen('}', r#", "status":"transcoded"}"#, 1),
        valid.replace(
            "\"artifact_sha256\":",
            "\"artifact_sha256\":null, \"artifact_sha256\":",
        ),
        valid.replace("\"byte_size\":12", "\"byte_size\":1, \"byte_size\":12"),
        r#"["untranscodable"]"#.into(),
    ] {
        let outcome = classify_transcode_reply(200, reply.as_bytes());
        assert_eq!(outcome, TranscodeCall::Retry);
    }
    let completed = TranscodeCall::Completed {
        artifact_sha256: sha.to_ascii_lowercase(),
        byte_size: 12,
    };
    assert_eq!(classify_transcode_reply(200, valid.as_bytes()), completed);
}

#[test]
fn transcode_dispatch_binds_the_token_to_a_canonical_verify_round() {
    let key = format!("0000-{}", "a".repeat(64));
    let sha = sha256_hex(b"sample audio");
    let prefix = VerifyProgress::scratch_prefix(JOB, 1);
    let token = derive_transcode_staging(&sha, &key, JOB, &prefix).unwrap();
    let decide = |prefix, token| decide_transcode(JOB, USER, KEY, &key, &sha, prefix, token);
    assert!(decide(&prefix, &token).is_ok());
    assert!(decide(&VerifyProgress::scratch_prefix(JOB, 2), &token).is_err());
    for dir in ["r0", "r01", "r1/extra", "r1.backup"] {
        let invalid = format!("ingest-scratch/{JOB}/{dir}/");
        assert!(derive_transcode_staging(&sha, &key, JOB, &invalid).is_err());
    }
    assert!(verify_transcode_artifact(b"", &sha256_hex(b""), 0).is_err());
    assert!(verify_transcode_artifact(b"sample audio", &sha, 12).is_ok());
}

#[test]
fn transcode_selection_requires_every_audio_file_to_have_intact_staging() {
    let files = ["one.flac", "cover.jpg", "two.flac"].map(|name| ParsedFile {
        name: name.into(),
        is_par2: false,
        segment_ids: vec![],
        segment_bytes: vec![],
    });
    let staged_file = |index: usize, size_refused_blocks| StagedFile {
        key: VerifyProgress::file_key(JOB, 1, index, &files[index].name),
        size: 12,
        sha256: sha256_hex(b"sample audio"),
        missing_blocks: 0,
        size_refused_blocks,
        par2_set_id: None,
        spool: vec![],
        crc32: 0,
        missing: false,
    };
    let mut staged = BTreeMap::from([(0, staged_file(0, 0))]);
    assert!(select_transcode_inputs(&files, &staged, JOB, 1).is_err());
    staged.insert(2, staged_file(2, 1));
    assert!(select_transcode_inputs(&files, &staged, JOB, 1).is_err());
    staged.insert(2, staged_file(2, 0));
    let selected = select_transcode_inputs(&files, &staged, JOB, 1).unwrap();
    assert_eq!([selected[0].file_index, selected[1].file_index], [0, 2]);
}
