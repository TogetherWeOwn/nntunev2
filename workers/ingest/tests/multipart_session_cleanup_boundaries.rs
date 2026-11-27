use base64::{engine::general_purpose::STANDARD, Engine};
use nntune_core::{media_envelope::EnvelopeContext, origin_sealer::OriginSealer};
use nntune_ingest::{
    cleanup_dispatch::{CleanupClaim, CleanupKind},
    multipart::{
        resume_checkpoint::{matches_checkpoint, CheckpointPart, ResumeBinding, ResumeCheckpoint},
        session_completion::SessionCompletion,
        MultipartError, Plan, UploadPart, UploadTarget, VersionedCompletionReply,
    },
};
use sha2::{Digest, Sha256};

const SESSION: &str = "00000000-0000-4000-8000-000000000001";
const CLAIM: &str = "abcdef00-0000-4000-8000-000000000002";
const BUCKET: &str = "test-bucket";
const KEY: &str = "media/03030303030340038003030303030303/04040404040440048004040404040404";

fn id(byte: u8) -> [u8; 16] {
    let mut id = [byte; 16];
    id[6] = 0x40;
    id[8] = 0x80;
    id
}

fn binding(plan: Plan<'_>) -> ResumeBinding<'_> {
    ResumeBinding {
        plan,
        configuration: id(5),
        environment: "01010101010140018001010101010101",
        library: "02020202020240028002020202020202",
        session: SESSION,
        object: "03030303030340038003030303030303",
        generation: "04040404040440048004040404040404",
        envelope_scheme: 2,
    }
}

fn one_part(upload_id: &str) -> (ResumeBinding<'_>, EnvelopeContext, UploadPart<'_>) {
    let mut sealer = OriginSealer::new(1).unwrap();
    let target = UploadTarget {
        bucket: BUCKET,
        key: KEY,
        upload_id,
    };
    let plan = Plan::new(target, sealer.header(), 1).unwrap();
    let mut partition = plan.partition();
    assert!(partition
        .push(&sealer.seal_chunk(&[42]).unwrap())
        .unwrap()
        .is_none());
    let part = partition.finish().unwrap().unwrap();
    let context = EnvelopeContext {
        identities: [0, 1, 2, 3, 4, 5].map(id),
        plaintext_len: 1,
        header: sealer.header().to_bytes(),
    };
    (binding(plan), context, part)
}

#[cfg(feature = "upload-caller")]
#[test]
fn initiation_does_not_join_comment_split_upload_ids() {
    use nntune_ingest::multipart::initiation::InitiatedUpload;

    let verify = |value| {
        let xml = format!(
            "<InitiateMultipartUploadResult><Bucket>{BUCKET}</Bucket><Key>{KEY}</Key>\
             <UploadId>{value}</UploadId></InitiateMultipartUploadResult>"
        );
        InitiatedUpload::verify(id(5), BUCKET, KEY, 200, xml.as_bytes())
    };
    assert_eq!(
        verify("opaque+&amp;=").unwrap().target().upload_id,
        "opaque+&="
    );
    assert_eq!(
        verify("opaque<!-- split -->+&amp;=").err(),
        Some(MultipartError::InitiationReceipt)
    );
}

#[test]
fn checkpoint_requires_a_nonblank_session_even_if_both_bindings_agree() {
    let (binding, _, _) = one_part("opaque-upload");
    for (session, accepted) in [
        (SESSION, true),
        ("", false),
        ("  ", false),
        ("session\n", false),
    ] {
        let binding = ResumeBinding { session, ..binding };
        let checkpoint = ResumeCheckpoint {
            binding,
            parts: &[],
        };
        assert_eq!(matches_checkpoint(binding, &[], &checkpoint), accepted);
    }
}

#[test]
fn standalone_completion_verification_checks_the_request_identity() {
    for (upload_id, etag, expected_error) in [
        ("opaque+/=", "\"opaque&tag\"", None),
        (
            "invalid\nvalue",
            "\"opaque&tag\"",
            Some(MultipartError::CompletionIdentity),
        ),
        (
            "opaque+/=",
            "invalid\0tag",
            Some(MultipartError::PartReceipt),
        ),
    ] {
        let (binding, context, part) = one_part(upload_id);
        let (_, number, bytes, checksum) = part.request();
        let digest = Sha256::digest(bytes);
        let checkpoint = ResumeCheckpoint {
            binding,
            parts: &[CheckpointPart {
                number,
                len: bytes.len(),
                etag,
                digest: digest.into(),
            }],
        };
        let receipt = part.verify_reply(etag.into(), checksum).unwrap();
        let completion =
            SessionCompletion::prepare(binding, &context, vec![receipt], &checkpoint).unwrap();
        let whole_checksum = STANDARD.encode(Sha256::digest(digest));
        let xml = format!(
            "<CompleteMultipartUploadResult><Bucket>{BUCKET}</Bucket><Key>{KEY}</Key>\
             <ChecksumSHA256>{whole_checksum}-1</ChecksumSHA256></CompleteMultipartUploadResult>"
        );
        assert_eq!(
            completion
                .verify_completion(VersionedCompletionReply {
                    configuration: id(5),
                    status: 200,
                    body: xml.as_bytes(),
                    version_headers: &["version"],
                })
                .err(),
            expected_error,
        );
    }
}

#[test]
fn cleanup_requires_canonical_committed_row_ids() {
    let claim = |session: &str, token: &str| {
        CleanupClaim::from_committed_row(
            session.into(),
            CleanupKind::AbsoluteExpiry,
            token.into(),
            86400,
        )
    };
    assert!(claim(SESSION, CLAIM).is_some());
    assert!(claim(SESSION, "00000000-0000-0000-0000-000000000000").is_none());
    assert!(claim(SESSION, &CLAIM.to_ascii_uppercase()).is_none());
    assert!(claim(&CLAIM.to_ascii_uppercase(), CLAIM).is_none());
}
