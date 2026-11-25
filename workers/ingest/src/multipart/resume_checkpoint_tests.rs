use super::super::{UploadPart, UploadTarget, PART_BYTES};
use super::*;
use nntune_core::{media::PLAIN_CHUNK_SIZE, origin_sealer::OriginSealer};
use sha2::{Digest, Sha256};

const TARGET: UploadTarget<'static> = UploadTarget {
    bucket: "test",
    key: "opaque",
    upload_id: "upload",
};

fn fixture_parts(
    target: UploadTarget<'static>,
) -> (ResumeBinding<'static>, Vec<UploadPart<'static>>) {
    let mut sealer = OriginSealer::new(PART_BYTES as u64).unwrap();
    let plan = Plan::new(target, sealer.header(), PART_BYTES as u64).unwrap();
    let binding = ResumeBinding {
        plan,
        configuration: [5; 16],
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
    (binding, parts)
}

fn confirm(
    part: &UploadPart<'static>,
    etag: &'static str,
) -> (UploadedPart<'static>, CheckpointPart<'static>) {
    let (_, number, bytes, checksum) = part.request();
    let receipt = part.verify_reply(etag.into(), checksum).unwrap();
    let saved = CheckpointPart {
        number,
        len: bytes.len(),
        etag,
        digest: Sha256::digest(bytes).into(),
    };
    (receipt, saved)
}

fn fixture() -> (
    ResumeBinding<'static>,
    Vec<UploadedPart<'static>>,
    Vec<CheckpointPart<'static>>,
) {
    let (binding, parts) = fixture_parts(TARGET);
    let (receipts, saved) = parts
        .iter()
        .zip(["opaque-1", "opaque-2"])
        .map(|(part, etag)| confirm(part, etag))
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
    assert!(!matches(ResumeBinding {
        configuration: [6; 16],
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

#[test]
fn late_confirmation_cannot_replace_the_current_part() {
    use super::super::session_completion::{PrepareError, SessionCompletion};

    let id = |byte| {
        let mut value = [byte; 16];
        value[6] = 0x40;
        value[8] = 0x80;
        value
    };
    let [environment, library, object, generation] = [1, 2, 3, 4].map(|n| hex::encode(id(n)));
    let target = UploadTarget {
        bucket: "test-bucket",
        key: "media/03030303030340038003030303030303/04040404040440048004040404040404",
        ..TARGET
    };
    let (original, original_parts) = fixture_parts(target);
    let restart_target = UploadTarget {
        upload_id: "restarted-upload",
        ..target
    };
    let (restarted, restarted_parts) = fixture_parts(restart_target);
    for (case, binding, parts) in [
        ("resume", original, &original_parts),
        ("restart", restarted, &restarted_parts),
    ] {
        let context = EnvelopeContext {
            identities: [0, 1, 2, 3, 4, 5].map(id),
            plaintext_len: binding.plan.plaintext_len,
            header: binding.plan.header,
        };
        let binding = ResumeBinding {
            configuration: id(5),
            environment: &environment,
            library: &library,
            object: &object,
            generation: &generation,
            ..binding
        };
        let first = || confirm(&parts[0], "opaque-1");
        let (confirmed_first, saved_first) = first();
        let prefix = ResumeCheckpoint {
            binding,
            parts: &[saved_first],
        };
        assert!(matches_checkpoint(binding, &[confirmed_first], &prefix));

        let (current, saved_current) = confirm(&parts[1], "opaque-current");
        let checkpoint = ResumeCheckpoint {
            binding,
            parts: &[saved_first, saved_current],
        };
        // The late reply is valid for the abandoned request. On resume it
        // even covers the same bytes and upload ID as the current reply.
        let (late, _) = confirm(&original_parts[1], "opaque-abandoned");
        assert_eq!(
            SessionCompletion::prepare(binding, &context, vec![first().0, late], &checkpoint).err(),
            Some(PrepareError::CheckpointMismatch),
            "{case} accepted the abandoned confirmation"
        );

        let completion =
            SessionCompletion::prepare(binding, &context, vec![first().0, current], &checkpoint)
                .unwrap();
        assert!(completion.manifest().eq([
            (1, saved_first.etag, parts[0].request().3),
            (2, saved_current.etag, parts[1].request().3),
        ]));
    }
}
