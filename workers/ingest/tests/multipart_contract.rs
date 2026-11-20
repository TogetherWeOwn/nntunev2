use base64::{engine::general_purpose::STANDARD, Engine};
use nntune_core::media::{HEADER_SIZE, PLAIN_CHUNK_SIZE};
use nntune_core::origin_sealer::OriginSealer;
use nntune_ingest::multipart::{
    Completion, MultipartError, Plan, UploadPart, UploadTarget, MAX_COMPLETION_BODY, PART_BYTES,
};
use sha2::{Digest, Sha256};

const TARGET: UploadTarget<'static> = UploadTarget {
    bucket: "test",
    key: "opaque",
    upload_id: "fixture",
};
const SUCCESS: &str = "CompleteMultipartUploadResult";

fn fixture(len: usize) -> (Plan<'static>, Vec<UploadPart<'static>>, Vec<u8>) {
    let mut sealer = OriginSealer::new(len as u64).unwrap();
    let plan = Plan::new(TARGET, sealer.header(), len as u64).unwrap();
    let mut wire = sealer.header().to_bytes().to_vec();
    let mut partition = plan.partition();
    let mut parts = Vec::new();
    let plaintext = vec![0x42; len];
    for plain in plaintext.chunks(PLAIN_CHUNK_SIZE as usize) {
        let mut chunk = sealer.seal_chunk(plain).unwrap();
        wire.extend_from_slice(&chunk.bytes);
        // A transport checksum must not use the origin chunk's digest.
        chunk.digest = [0; 32];
        parts.extend(partition.push(&chunk).unwrap());
    }
    parts.extend(partition.finish().unwrap());
    (plan, parts, wire)
}

struct FakeS3<'a>(&'a [u8]);

impl FakeS3<'_> {
    fn upload(&self, part: &UploadPart<'_>) -> (String, String) {
        let (target, number, body, checksum) = part.request();
        let expected = self.0.chunks(PART_BYTES).nth(number as usize - 1).unwrap();
        let digest = STANDARD.encode(Sha256::digest(expected));
        assert!(target == TARGET);
        assert_eq!((body, checksum), (expected, digest.as_str()));
        (format!("\"opaque-{number}\""), digest)
    }

    fn complete(&self, request: &Completion<'_>, prefixed: bool) -> String {
        assert!(request.object() == (TARGET, self.0.len() as u64));
        let chunks = self.0.chunks(PART_BYTES);
        let count = chunks.len();
        assert_eq!(request.manifest().count(), count);
        let mut hashes = Vec::new();
        for (i, ((number, etag, checksum), bytes)) in request.manifest().zip(chunks).enumerate() {
            let hash = Sha256::digest(bytes);
            assert_eq!(number as usize, i + 1);
            assert_eq!(etag, format!("\"opaque-{number}\""));
            assert_eq!(checksum, STANDARD.encode(hash));
            hashes.extend_from_slice(&hash);
        }
        let composite = STANDARD.encode(Sha256::digest(hashes));
        let bom = if prefixed { "\u{feff}" } else { "" };
        let (p, ns) = if prefixed { ("s3:", ":s3") } else { ("", "") };
        let s3 = "http://s3.amazonaws.com/doc/2006-03-01/";
        format!("{bom} \n<{p}{SUCCESS} xmlns{ns}=\"{s3}\"><{p}Key xmlns{ns}=\"urn:fixture\">a&amp;b&#x2E;flac</{p}Key><{p}ChecksumSHA256>{composite}-{count}</{p}ChecksumSHA256></{p}{SUCCESS}>")
    }
}

#[test]
fn exact_wire_partition_and_retry() {
    // The next byte past this boundary is part of the final authentication tag.
    let boundary = 80 * PLAIN_CHUNK_SIZE as usize - HEADER_SIZE as usize;
    for (len, count) in [(boundary - 1, 1), (boundary, 1), (boundary + 1, 2)] {
        let (plan, parts, wire) = fixture(len);
        assert_eq!(parts.len(), count);
        let fake = FakeS3(&wire);
        let mut receipts = Vec::new();
        for part in &parts {
            let (etag, checksum) = fake.upload(part);
            assert_eq!(fake.upload(part), (etag.clone(), checksum.clone()));
            receipts.push(part.verify_reply(etag, &checksum).unwrap());
        }
        let completion = plan.complete(receipts).unwrap();
        completion
            .verify_reply(200, fake.complete(&completion, count > 1).as_bytes())
            .unwrap();
    }
}

#[test]
fn rejects_bad_receipts_and_false_completion_success() {
    use MultipartError::*;
    let (plan, parts, wire) = fixture(1);
    let fake = FakeS3(&wire);
    let (etag, checksum) = fake.upload(&parts[0]);
    let invalid = parts[0].verify_reply(etag.clone(), &etag);
    assert!(matches!(invalid, Err(Checksum)));
    assert!(matches!(plan.partition().finish(), Err(Geometry)));
    assert!(matches!(plan.complete(Vec::new()), Err(Geometry)));
    let receipt = parts[0].verify_reply(etag, &checksum).unwrap();
    let completion = plan.complete(vec![receipt]).unwrap();
    let success = fake.complete(&completion, false);
    let non_ok = completion.verify_reply(503, success.as_bytes());
    assert_eq!(non_ok, Err(CompletionStatus));
    let duplicate_prefix = format!("<{SUCCESS} xmlns:a='x' xmlns:a='y'/>");
    for (body, error) in [
        (" \n<Error/>".into(), EmbeddedError),
        (format!("<{SUCCESS}><Error/></{SUCCESS}>"), EmbeddedError),
        (success.replace("-1</", "-2</"), Checksum),
        (success.trim_end_matches('>').into(), CompletionBody),
        (format!("<{SUCCESS} xmlns='' xmlns=''/>"), CompletionBody),
        (duplicate_prefix, CompletionBody),
        (success.replace("&amp;", "&undefined;"), CompletionBody),
        (success.replace("&#x2E;", "&#xD800;"), CompletionBody),
        (format!("{success}<!-- bad--comment -->"), CompletionBody),
        (format!("<!DOCTYPE {SUCCESS}>{success}"), CompletionBody),
        (" ".repeat(MAX_COMPLETION_BODY + 1), CompletionBody),
    ] {
        assert_eq!(completion.verify_reply(200, body.as_bytes()), Err(error));
    }
}
