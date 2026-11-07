use super::*;

#[test]
fn aad_tuple_and_purpose_are_pinned_and_payload_is_wrap_version_independent() {
    let ctx = context("row", "owner");
    let mut expected = Vec::new();
    for value in [
        "nntune-envelope-v2",
        "payload",
        "ingest_preparation_plans",
        "plan",
        "owner",
        "row",
    ] {
        expected.extend_from_slice(&(value.len() as u32).to_be_bytes());
        expected.extend_from_slice(value.as_bytes());
    }
    expected.extend_from_slice(&2_u16.to_be_bytes());
    expected.extend_from_slice(&1_u16.to_be_bytes());
    assert!(ctx.aad("payload", None) == expected);
    let mut wrap = Vec::new();
    for value in [
        "nntune-envelope-v2",
        "dek",
        "ingest_preparation_plans",
        "plan",
        "owner",
        "row",
    ] {
        wrap.extend_from_slice(&(value.len() as u32).to_be_bytes());
        wrap.extend_from_slice(value.as_bytes());
    }
    wrap.extend_from_slice(&2_u16.to_be_bytes());
    wrap.extend_from_slice(&1_u16.to_be_bytes());
    wrap.extend_from_slice(&7_u32.to_be_bytes());
    assert!(ctx.aad("dek", Some(7)) == wrap);
    assert!(ctx.aad("dek", Some(7)) != ctx.aad("dek", Some(8)));
    assert_eq!(PlanMode::default(), PlanMode::Disabled);
}

#[test]
fn field_envelope_substitution_across_every_context_dimension_refuses() {
    let key =
        Zeroizing::new(crate::random::secure_random_bytes::<32>().expect("test key randomness"));
    let bytes = Zeroizing::new(
        crate::random::secure_random_bytes::<79>()
            .expect("test payload randomness")
            .to_vec(),
    );
    let ctx = context("row", "owner");
    let sealed = field_envelope::seal(&key, 1, &ctx, &bytes).expect("seal fixture");
    let opened =
        Zeroizing::new(field_envelope::open(&key, 1, &ctx, &sealed).expect("open fixture"));
    assert!(opened.as_slice() == bytes.as_slice());
    for dimension in 0..6 {
        let mut other = context("row", "owner");
        match dimension {
            0 => other.row = "another-row",
            1 => other.scope = "another-owner",
            2 => other.table = "another-table",
            3 => other.field = "another-field",
            4 => other.version = 1,
            _ => other.format = 2,
        }
        assert!(field_envelope::open(&key, 1, &other, &sealed).is_err());
    }
    assert!(field_envelope::open(&key, 2, &ctx, &sealed).is_err());
    let mut changed = sealed.clone();
    changed.ciphertext[0] ^= 1;
    assert!(field_envelope::open(&key, 1, &ctx, &changed).is_err());
}

#[test]
fn framed_identity_is_stable_and_binds_job_operation_and_file() {
    let original = "0".repeat(64);
    let mut request = PlanRequest {
        owner_id: "owner",
        job_id: "job",
        operation_key: "operation",
        file_index: 0,
        original_sha256: &original,
        original_bytes: 79,
        file_count: 2,
    };
    let first = request.id().expect("valid identity");
    assert!(first == request.id().expect("stable identity"));
    request.file_index = 1;
    assert!(first != request.id().expect("second file identity"));
    request.file_index = 0;
    request.operation_key = "another-operation";
    assert!(first != request.id().expect("second operation identity"));
    request.operation_key = "operation";
    request.job_id = "another-job";
    assert!(first != request.id().expect("second job identity"));
    request.operation_key = "not valid";
    assert!(matches!(request.id(), Err(PlanError::Invalid)));
}
