use base64::{engine::general_purpose::STANDARD, Engine};
use nntune_core::media::{HEADER_SIZE, PLAIN_CHUNK_SIZE};
use nntune_core::origin_sealer::OriginSealer;
use nntune_ingest::multipart::{
    Completion, MultipartError, Plan, UploadPart, UploadTarget, UploadedPart, MAX_COMPLETION_BODY,
    PART_BYTES,
};
use sha2::{Digest, Sha256};

const TARGET: UploadTarget<'static> = UploadTarget {
    bucket: "test",
    key: "opaque",
    upload_id: "fixture",
};
const SUCCESS: &str = "CompleteMultipartUploadResult";
const S3_NS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

struct FakeS3(Vec<u8>);

impl FakeS3 {
    fn fixture(len: usize) -> (Self, Plan<'static>, Vec<UploadPart<'static>>) {
        let mut sealer = OriginSealer::new(len as u64).unwrap();
        let plan = Plan::new(TARGET, sealer.header(), len as u64).unwrap();
        let mut wire = sealer.header().to_bytes().to_vec();
        let mut partition = plan.partition();
        let mut parts = Vec::new();
        for plain in vec![0x42; len].chunks(PLAIN_CHUNK_SIZE as usize) {
            let mut chunk = sealer.seal_chunk(plain).unwrap();
            wire.extend_from_slice(&chunk.bytes);
            chunk.digest = [0; 32]; // Transport must hash wire bytes, not codec digests.
            parts.extend(partition.push(&chunk).unwrap());
        }
        parts.extend(partition.finish().unwrap());
        (Self(wire), plan, parts)
    }

    fn upload<'a>(&self, part: &UploadPart<'a>, number: u32) -> UploadedPart<'a> {
        let expected = self.0.chunks(PART_BYTES).nth(number as usize - 1).unwrap();
        let digest = STANDARD.encode(Sha256::digest(expected));
        assert!(part.request() == (TARGET, number, expected, digest.as_str()));
        part.verify_reply(format!("\"opaque-{number}\""), &digest)
            .unwrap()
    }

    fn complete(&self, request: &Completion<'_>, prefixed: bool) -> String {
        assert!(request.object() == (TARGET, self.0.len() as u64));
        let chunks = self.0.chunks(PART_BYTES);
        let count = chunks.len();
        assert_eq!(request.manifest().count(), count);
        let mut composite = Sha256::new();
        for (i, ((number, etag, checksum), bytes)) in request.manifest().zip(chunks).enumerate() {
            let hash = Sha256::digest(bytes);
            assert_eq!(number as usize, i + 1);
            assert_eq!(etag, format!("\"opaque-{number}\""));
            assert_eq!(checksum, STANDARD.encode(hash));
            composite.update(hash);
        }
        let digest = STANDARD.encode(composite.finalize());
        let (p, ns) = if prefixed { ("s3:", ":s3") } else { ("", "") };
        format!(" \n<{p}{SUCCESS} xmlns{ns}=\"{S3_NS}\"><{p}Key xmlns{ns}=\"{S3_NS}\">a&amp;b&#x2E;flac</{p}Key><{p}ChecksumSHA256>{digest}-{count}</{p}ChecksumSHA256></{p}{SUCCESS}>")
    }
}

#[test]
fn exact_wire_partition_and_retry() {
    // The next byte past this boundary is part of the final authentication tag.
    let boundary = 80 * PLAIN_CHUNK_SIZE as usize - HEADER_SIZE as usize;
    for (len, count) in [(boundary - 1, 1), (boundary, 1), (boundary + 1, 2)] {
        let (fake, plan, parts) = FakeS3::fixture(len);
        assert_eq!(parts.len(), count);
        let mut receipts = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            let number = i as u32 + 1;
            fake.upload(part, number);
            receipts.push(fake.upload(part, number));
        }
        let completion = plan.complete(receipts).unwrap();
        let body = fake.complete(&completion, count > 1);
        completion
            .verify_reply(200, format!("\u{feff}{body}").as_bytes())
            .unwrap();
    }
}

#[test]
fn rejects_bad_receipts_and_false_completion_success() {
    use MultipartError::*;
    let (fake, plan, parts) = FakeS3::fixture(1);
    let invalid = parts[0].verify_reply("opaque".into(), "opaque");
    assert!(matches!(invalid, Err(Checksum)));
    assert!(matches!(plan.partition().finish(), Err(Geometry)));
    assert!(matches!(plan.complete(Vec::new()), Err(Geometry)));
    let completion = plan.complete(vec![fake.upload(&parts[0], 1)]).unwrap();
    let success = fake.complete(&completion, true);
    let attr = |a| success.replacen("<s3:Key", &format!("<s3:Key {a}"), 1);
    let bind = |uri| attr(format!("xmlns:p='{uri}'"));
    let verify = |status, body: &str| completion.verify_reply(status, body.as_bytes());
    assert_eq!(verify(503, &success), Err(CompletionStatus));
    assert_eq!(verify(200, " \n<Error/>"), Err(EmbeddedError));
    let error = success.replace("s3:Key", "s3:Error");
    assert_eq!(verify(200, &error), Err(EmbeddedError));
    assert_eq!(verify(200, &success.replace("-1</", "-2</")), Err(Checksum));
    for body in [
        attr("unexpected='<'".into()),
        attr(format!("xmlns:s3='{S3_NS}'")),
        bind("http://www.w3.org/XML/1998/namespace"),
        bind("http://www.w3.org/2000/xmlns/"),
        success.replace("s3:Key", "s3:1Key"),
        success.trim_end_matches('>').into(),
        format!("{success}{success}"),
        success.replace("&amp;", "&undefined;"),
        success.replace("&#x2E;", "&#xD800;"),
        format!("{success}<!-- bad--comment -->"),
        format!("<!DOCTYPE {SUCCESS}>{success}"),
        " ".repeat(MAX_COMPLETION_BODY + 1),
    ] {
        assert_eq!(verify(200, &body), Err(CompletionBody));
    }
}
