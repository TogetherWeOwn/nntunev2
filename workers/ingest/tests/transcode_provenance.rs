//! Per-artifact derivation is provenance, never a cache grant. These synthetic
//! records model trusted checkpoint writers, not cryptographically signed proof.

use std::collections::BTreeSet;

use nntune_ingest::dispatch::content_sha256_hex;
use nntune_ingest::pool_provenance::{PoolProvenance, MAX_CONTRIBUTORS};
use nntune_ingest::transcode::{
    decide_transcode, TranscodeInput, TranscodeOutput, TranscodeRequest, MAX_ARTIFACT_BYTES,
};

const JOB: &str = "job_0123456789abcdef";
const A: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const B: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const SOURCE: &[u8] = b"verified lossless source";
const ARTIFACT: &[u8] = b"encoded opus output";

fn input(providers: &[&str]) -> TranscodeInput {
    let sha = content_sha256_hex(SOURCE);
    let proof = (!providers.is_empty()).then(|| {
        serde_json::from_value::<PoolProvenance>(serde_json::json!({
            "sha256": sha, "size": SOURCE.len(), "provider_ids": providers
        }))
        .unwrap()
    });
    TranscodeInput {
        file_index: 3,
        key: format!("ingest-scratch/{JOB}/r1/in/0003-{}", "a".repeat(64)),
        sha256: sha,
        size: SOURCE.len() as u64,
        scratch_prefix: format!("ingest-scratch/{JOB}/r1/"),
        pool_provenance: proof,
    }
}

fn request(input: &TranscodeInput) -> TranscodeRequest {
    decide_transcode(
        JOB,
        "user_01",
        "ingest:user_01:content:upload",
        input.key.rsplit('/').next().unwrap(),
        &input.sha256,
        &input.scratch_prefix,
        &input.staging_id(JOB).unwrap(),
    )
    .unwrap()
}

fn output(input: &TranscodeInput) -> TranscodeOutput {
    TranscodeOutput::verified(
        &request(input),
        input,
        ARTIFACT,
        &content_sha256_hex(ARTIFACT),
        ARTIFACT.len() as u64,
    )
    .unwrap()
}

#[test]
fn artifact_keeps_all_source_contributors_without_relabelling_source_bytes() {
    for ids in [&[A][..], &[A, B][..]] {
        let input = input(ids);
        let out = output(&input);
        let expected: BTreeSet<String> = ids.iter().map(|id| id.to_string()).collect();
        assert_eq!(out.contributing_provider_ids(JOB, &input), Some(&expected));
        assert_eq!(out.artifact_sha256(), content_sha256_hex(ARTIFACT));
        assert_eq!(out.byte_size(), ARTIFACT.len() as u64);
        let json = serde_json::to_value(&out).unwrap();
        assert_eq!(json["source"]["pool_provenance"]["sha256"], input.sha256);
        assert_ne!(
            json["source"]["pool_provenance"]["sha256"],
            out.artifact_sha256()
        );
        assert_eq!(json.as_object().unwrap().len(), 4);
        let back: TranscodeOutput = serde_json::from_value(json).unwrap();
        assert_eq!(back, out);
        assert!(back.matches_input(JOB, &input));
    }
}

#[test]
fn missing_stale_and_invalid_source_evidence_remain_unattributed() {
    let mut cases = vec![input(&[]), input(&[A]), input(&["bad-id"])];
    cases[1].pool_provenance = PoolProvenance::from_body(A, b"other bytes");
    for input in cases {
        let out = output(&input);
        assert!(out.matches_input(JOB, &input));
        assert_eq!(out.contributing_provider_ids(JOB, &input), None);
        assert!(serde_json::to_value(out).unwrap()["source"]
            .get("pool_provenance")
            .is_none());
    }
}

#[test]
fn complete_contributor_ceiling_is_preserved_not_truncated() {
    let ids: Vec<String> = (0..=MAX_CONTRIBUTORS)
        .map(|n| format!("{n:08x}-0000-4000-8000-000000000000"))
        .collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let at_bound = input(&refs[..MAX_CONTRIBUTORS]);
    let out = output(&at_bound);
    assert_eq!(
        out.contributing_provider_ids(JOB, &at_bound).unwrap().len(),
        MAX_CONTRIBUTORS
    );
    let overflow = input(&refs);
    assert_eq!(
        output(&overflow).contributing_provider_ids(JOB, &overflow),
        None
    );
}

#[test]
fn source_layout_substitution_is_rejected_before_dispatch() {
    let base = input(&[A]);
    let mut cases = Vec::new();
    let mut changed = base.clone();
    changed.key = changed.key.replace(JOB, "job_other");
    cases.push(changed);
    let mut changed = base.clone();
    changed.key = changed.key.replace("/r1/", "/r2/");
    cases.push(changed);
    let mut changed = base.clone();
    changed.file_index += 1;
    cases.push(changed);
    let mut changed = base.clone();
    changed.file_index = nntune_ingest::verify::MAX_VERIFY_FILES;
    cases.push(changed);
    let mut changed = base.clone();
    changed.size = 0;
    cases.push(changed);
    for changed in cases {
        assert!(changed.staging_id(JOB).is_err(), "{changed:?}");
    }
}

#[test]
fn output_refuses_other_request_generation_or_source() {
    let input = input(&[A]);
    let req = request(&input);
    let mut cases = Vec::new();
    let mut changed = req.clone();
    changed.job_id = "job_other".into();
    cases.push(changed);
    let mut changed = req.clone();
    changed.input_key = changed.input_key.replace("0003", "0004");
    cases.push(changed);
    let mut changed = req.clone();
    changed.scratch_prefix = changed.scratch_prefix.replace("/r1/", "/r2/");
    cases.push(changed);
    let mut changed = req.clone();
    changed.staging_id = "a".repeat(64);
    cases.push(changed);
    let mut changed = req;
    changed.source_sha256 = "b".repeat(64);
    cases.push(changed);
    for req in cases {
        assert!(TranscodeOutput::verified(
            &req,
            &input,
            ARTIFACT,
            &content_sha256_hex(ARTIFACT),
            ARTIFACT.len() as u64
        )
        .is_err());
    }
}

#[test]
fn output_requires_actual_bytes_and_profile_size_before_it_can_be_recorded() {
    let input = input(&[A]);
    let req = request(&input);
    let sha = content_sha256_hex(ARTIFACT);
    for (bytes, hash, size) in [
        (&b"different bytes"[..], sha.as_str(), ARTIFACT.len() as u64),
        (ARTIFACT, "bad-hash", ARTIFACT.len() as u64),
        (ARTIFACT, sha.as_str(), ARTIFACT.len() as u64 + 1),
        (&b""[..], sha.as_str(), 0),
        (ARTIFACT, sha.as_str(), MAX_ARTIFACT_BYTES + 1),
    ] {
        assert!(TranscodeOutput::verified(&req, &input, bytes, hash, size).is_err());
    }
    let upper = TranscodeOutput::verified(
        &req,
        &input,
        ARTIFACT,
        &sha.to_ascii_uppercase(),
        ARTIFACT.len() as u64,
    )
    .unwrap();
    assert_eq!(upper.artifact_sha256(), sha);
}

#[test]
fn persisted_output_cannot_be_applied_to_another_job_round_source_or_file() {
    let input = input(&[A, B]);
    let out = output(&input);
    assert!(!out.matches_input("job_other", &input));
    let value = serde_json::to_value(&input).unwrap();
    for (field, replacement) in [
        ("file_index", serde_json::json!(4)),
        ("sha256", serde_json::json!("b".repeat(64))),
        ("size", serde_json::json!(input.size + 1)),
        (
            "scratch_prefix",
            serde_json::json!(format!("ingest-scratch/{JOB}/r2/")),
        ),
        (
            "key",
            serde_json::json!(input.key.replace("0003-", "0004-")),
        ),
        ("pool_provenance", serde_json::Value::Null),
    ] {
        let mut changed = value.clone();
        changed[field] = replacement;
        let changed: TranscodeInput = serde_json::from_value(changed).unwrap();
        assert!(!out.matches_input(JOB, &changed), "{field}");
        assert_eq!(out.contributing_provider_ids(JOB, &changed), None);
    }
}

#[test]
fn malformed_persisted_output_never_supplies_contributors() {
    let input = input(&[A]);
    let value = serde_json::to_value(output(&input)).unwrap();
    for (field, replacement) in [
        ("staging_id", serde_json::json!("b".repeat(64))),
        ("artifact_sha256", serde_json::json!("BAD")),
        (
            "artifact_sha256",
            serde_json::json!(content_sha256_hex(ARTIFACT).to_uppercase()),
        ),
        ("byte_size", serde_json::json!(0)),
        ("byte_size", serde_json::json!(MAX_ARTIFACT_BYTES + 1)),
    ] {
        let mut changed = value.clone();
        changed[field] = replacement;
        let out: TranscodeOutput = serde_json::from_value(changed).unwrap();
        assert!(!out.matches_input(JOB, &input), "{field}");
        assert_eq!(out.contributing_provider_ids(JOB, &input), None);
    }
    let mut extra = value;
    extra["grant"] = serde_json::json!(true);
    assert!(serde_json::from_value::<TranscodeOutput>(extra).is_err());
}
