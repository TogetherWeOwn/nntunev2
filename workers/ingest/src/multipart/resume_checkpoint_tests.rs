use super::super::{UploadTarget, PART_BYTES};
use super::*;
use nntune_core::{media::PLAIN_CHUNK_SIZE, origin_sealer::OriginSealer};
use sha2::{Digest, Sha256};

fn fixture() -> (
    ResumeBinding<'static>,
    Vec<UploadedPart<'static>>,
    Vec<CheckpointPart<'static>>,
) {
    let mut sealer = OriginSealer::new(PART_BYTES as u64).unwrap();
    let target = UploadTarget {
        bucket: "test",
        key: "opaque",
        upload_id: "upload",
    };
    let plan = Plan::new(target, sealer.header(), PART_BYTES as u64).unwrap();
    let binding = ResumeBinding {
        plan,
        environment: "test",
        library: "library",
        session: "session",
        object: "object",
        generation: "generation",
        envelope_scheme: 2,
    };
    let mut partition = plan.partition();
    let mut parts = Vec::new();
    for chunk in vec![0x42; PART_BYTES].chunks(PLAIN_CHUNK_SIZE as usize) {
        parts.extend(partition.push(&sealer.seal_chunk(chunk).unwrap()).unwrap());
    }
    parts.extend(partition.finish().unwrap());
    assert_eq!(parts.len(), 2);
    let (receipts, saved) = parts
        .iter()
        .map(|part| {
            let (_, number, bytes, checksum) = part.request();
            let etag = if number == 1 { "opaque-1" } else { "opaque-2" };
            let receipt = part.verify_reply(etag.into(), checksum).unwrap();
            let saved = CheckpointPart {
                number,
                len: bytes.len(),
                etag,
                digest: Sha256::digest(bytes).into(),
            };
            (receipt, saved)
        })
        .unzip();
    (binding, receipts, saved)
}

#[test]
fn requires_exact_upload_session_and_object_binding() {
    let (binding, receipts, parts) = fixture();
    let checkpoint = |binding| ResumeCheckpoint {
        binding,
        parts: &parts,
    };
    let matches = |saved| matches_checkpoint(binding, &receipts, &checkpoint(saved));
    assert!(matches(binding));
    assert!(!matches(ResumeBinding {
        session: "other",
        ..binding
    }));
    assert!(!matches(ResumeBinding {
        generation: "other",
        ..binding
    }));
    let legacy = ResumeBinding {
        envelope_scheme: 1,
        ..binding
    };
    assert!(!matches(legacy));
    assert!(!matches_checkpoint(legacy, &receipts, &checkpoint(legacy)));

    let mut other_upload = binding;
    other_upload.plan.target.upload_id = "other";
    assert!(!matches(other_upload));
    // Even with matching bindings, receipts must belong to that plan.
    assert!(!matches_checkpoint(
        other_upload,
        &receipts,
        &checkpoint(other_upload)
    ));
}

#[test]
fn requires_exact_ordered_part_prefix() {
    let (binding, mut receipts, parts) = fixture();
    let matches = |receipts: &[UploadedPart<'_>], parts: &[CheckpointPart<'_>]| {
        matches_checkpoint(binding, receipts, &ResumeCheckpoint { binding, parts })
    };
    assert!(matches(&[], &[]));
    assert!(matches(&receipts[..1], &parts[..1]));
    assert!(!matches(&receipts, &parts[..1]));
    let first = parts[0];
    let mut wrong_digest = first;
    wrong_digest.digest[0] ^= 1;
    for different in [
        CheckpointPart { number: 2, ..first },
        wrong_digest,
        CheckpointPart {
            etag: "other",
            ..first
        },
        CheckpointPart {
            len: first.len + 1,
            ..first
        },
    ] {
        assert!(!matches(&receipts, &[different, parts[1]]));
    }
    receipts.swap(0, 1);
    assert!(!matches(&receipts, &[parts[1], first]));
}
