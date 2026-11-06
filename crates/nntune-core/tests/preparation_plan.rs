//! Independent binary format vectors. Synthetic metadata, no posting material.
use nntune_core::preparation_plan::{PlanReference, PreparationPlan, PLAN_BYTE_CAP};

fn plan(size: u64) -> PreparationPlan {
    PreparationPlan {
        owner: "o".into(),
        job: "j".into(),
        operation: "p".into(),
        predecessor: 0,
        original_digest: [2; 32],
        original_bytes: 9,
        file_count: 1,
        file_index: 0,
        object: "t".into(),
        object_version: "v".into(),
        plaintext_bytes: size,
        plaintext_digest: [3; 32],
        blocks_per_article: 1,
    }
}

#[test]
fn one_byte_golden_vector_pins_order_width_endianness_and_header_geometry() {
    let mut golden = b"NNPREP01\x01o\x01j\x01p\x01".to_vec();
    golden.extend([0; 8]);
    golden.extend([2; 32]);
    golden.extend(9_u64.to_be_bytes());
    golden.extend(1_u32.to_be_bytes());
    golden.extend(0_u32.to_be_bytes());
    golden.extend(b"\x01t\x01v");
    golden.extend(1_u64.to_be_bytes());
    golden.extend([3; 32]);
    golden.push(1);
    golden.extend(1_u32.to_be_bytes());
    golden.extend(49_u64.to_be_bytes()); // 32-byte header + one byte + tag.
    golden.extend(1_u32.to_be_bytes());
    for n in [0_u64, 1, 32, 17] {
        golden.extend(n.to_be_bytes());
    }
    assert_eq!(plan(1).encode().unwrap(), golden);
    let decoded = PreparationPlan::decode(&golden).unwrap();
    assert!(decoded == plan(1));
    assert_eq!(decoded.encode().unwrap(), golden);
}

#[test]
fn boundary_files_and_article_policy_roundtrip_without_gaps() {
    for size in [1, 65535, 65536, 65537, 65536 * 11, 65536 * 11 + 1] {
        for policy in [1, 11, u32::MAX] {
            let mut p = plan(size);
            p.blocks_per_article = policy;
            let encoded = p.encode().unwrap();
            assert!(PreparationPlan::decode(&encoded).unwrap() == p);
        }
    }
}

#[test]
fn unknown_versions_purpose_layout_counts_and_every_truncation_refuse() {
    let bytes = plan(1).encode().unwrap();
    for len in 0..bytes.len() {
        assert!(PreparationPlan::decode(&bytes[..len]).is_err());
    }
    // Byte positions independently follow the golden vector above.
    for index in [0, 7, 14, 115] {
        let mut b = bytes.clone();
        b[index] = 9;
        assert!(PreparationPlan::decode(&b).is_err());
    }
    let mut trailing = bytes;
    trailing.push(0);
    assert!(PreparationPlan::decode(&trailing).is_err());
    assert!(PreparationPlan::decode(&vec![0; PLAN_BYTE_CAP + 1]).is_err());
}

#[test]
fn invalid_identity_numeric_and_count_ranges_refuse_before_geometry_allocation() {
    for n in 0..10 {
        let mut p = plan(1);
        match n {
            0 => p.owner.clear(),
            1 => p.job = "x".repeat(65),
            2 => p.operation = "not/atom".into(),
            3 => p.predecessor = i64::MAX as u64,
            4 => p.original_bytes = 0,
            5 => p.file_count = 0,
            6 => p.file_index = 1,
            7 => p.plaintext_bytes = 0,
            8 => p.plaintext_bytes = (1_u64 << 53) - 1,
            _ => p.blocks_per_article = 0,
        }
        assert!(p.encode().is_err());
    }
}

#[test]
fn private_reference_never_accepts_a_trailer_or_unknown_frame_version() {
    let reference = PlanReference {
        id: "p".into(),
        version: "v".into(),
        digest: [0; 32],
        bytes: 7,
    };
    let encoded = reference.encode().unwrap();
    assert!(PlanReference::decode(&encoded).unwrap() == reference);
    for len in 0..encoded.len() {
        assert!(PlanReference::decode(&encoded[..len]).is_err());
    }
    let mut changed = encoded.clone();
    changed[0] = 2;
    assert!(PlanReference::decode(&changed).is_err());
    changed = encoded;
    changed.push(0);
    assert!(PlanReference::decode(&changed).is_err());
}
