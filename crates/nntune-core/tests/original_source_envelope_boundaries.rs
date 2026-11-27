use nntune_core::{
    digest_enrollment::DigestEnrollment,
    digest_geometry::DigestGeometry,
    digest_verifier::PageError,
    media::{ciphertext_size, PLAIN_CHUNK_SIZE},
    media_envelope::{
        verify_enrolled_scheme2, wrap_scheme2, EnrolledEnvelopeError, EnvelopeContext,
    },
    media_layout::{plan_articles, plan_range},
    origin_sealer::{OriginSealer, MAX_NEW_PLAINTEXT},
    original_source::{
        reader::{RangeReader, ReadControl},
        OriginalSource, SourceError, MAX_ARTICLE_BYTES, PAYLOAD_FORMAT,
    },
};

const TRACK: &str = "00000000-0000-0000-0000-000000000001";
const JOB: &str = "fixture";

fn source(parts: usize, size: u64) -> Result<OriginalSource, SourceError> {
    let articles: Vec<_> = (1..=parts)
        .map(|number| {
            serde_json::json!({
                "number": number,
                "reference": format!("fixture-{number}@example.invalid"),
                "encoded_bytes": 512,
            })
        })
        .collect();
    let record = serde_json::json!({
        "track_id": TRACK,
        "source_job_id": JOB,
        "file_ordinal": 0,
        "file_size": size,
        "article_count": parts,
        "articles": articles,
    });
    OriginalSource::parse(PAYLOAD_FORMAT, &record.to_string().into_bytes(), TRACK, JOB)
}

struct Live;

impl ReadControl for Live {
    fn check(&mut self) -> Result<(), SourceError> {
        Ok(())
    }
}

#[test]
fn stored_source_refuses_impossible_file_extents() {
    for (parts, size) in [(2, 1), (1, MAX_ARTICLE_BYTES as u64 + 1)] {
        assert_eq!(source(parts, size).err(), Some(SourceError::Bounds));
    }
    assert!(source(2, 2).is_ok());
}

#[test]
fn short_range_cannot_hide_a_gap_before_the_proven_final_part() {
    let source = source(2, 4).unwrap();
    let final_part = b"=ybegin part=2 total=2 size=4 name=fixture\n=ypart begin=4 end=4\nB\n=yend size=1 part=2\n";
    let short_prefix = b"=ybegin part=1 total=2 size=4 name=fixture\n=ypart begin=1 end=2\nAB\n=yend size=2 part=1\n";
    let exact_prefix = b"=ybegin part=1 total=2 size=4 name=fixture\n=ypart begin=1 end=3\nABC\n=yend size=3 part=1\n";

    for (prefix, valid) in [(short_prefix.as_slice(), false), (exact_prefix, true)] {
        let mut reader = RangeReader::new(&source, 0..1).unwrap();
        reader
            .push(final_part.as_slice(), &mut Live, |_| {
                panic!("premature output")
            })
            .unwrap();
        let mut output = Vec::new();
        let result = reader.push(prefix, &mut Live, |bytes| {
            output.extend_from_slice(bytes);
            Ok(())
        });
        if valid {
            assert_eq!(result, Ok(()));
            assert_eq!(output, [b'A' - 42]);
            assert_eq!(reader.next_reference(), Ok(None));
        } else {
            assert_eq!(result, Err(SourceError::Integrity));
            assert!(output.is_empty());
            assert_eq!(reader.next_reference(), Err(SourceError::Integrity));
        }
    }
}

#[test]
fn final_part_must_leave_space_for_each_prefix_article() {
    let source = source(3, 4).unwrap();
    let body = b"=ybegin part=3 total=3 size=4 name=fixture\n=ypart begin=2 end=4\nABC\n=yend size=3 part=3\n";
    let mut reader = RangeReader::new(&source, 0..1).unwrap();
    assert_eq!(
        reader.push(body.as_slice(), &mut Live, |_| panic!("premature output")),
        Err(SourceError::Integrity)
    );
    assert_eq!(reader.next_reference(), Err(SourceError::Integrity));
}

#[test]
fn original_reader_rejects_truncated_or_conflicting_frames() {
    let source = source(2, 2).unwrap();
    let valid = "=ybegin part=2 total=2 size=2 name=fixture part=9\r\n=ypart begin=2 end=2\r\nA\r\n=yend size=1 part=2\r\n";
    let wrong_trailer = valid.replace("part=2\r\n", "part=1\r\n");
    let wrong_total = valid.replace("total=2", "total=3");
    let valid = valid.as_bytes();
    let mut repeated = valid.to_vec();
    repeated.extend_from_slice(b"=yend size=1 part=2\r\n");
    let mut reader = RangeReader::new(&source, 0..1).unwrap();
    reader
        .push(valid, &mut Live, |_| panic!("premature output"))
        .unwrap();
    for bad in [
        &valid[..valid.len() - 2],
        wrong_trailer.as_bytes(),
        wrong_total.as_bytes(),
        &repeated,
    ] {
        let mut reader = RangeReader::new(&source, 0..1).unwrap();
        assert_eq!(
            reader.push(bad, &mut Live, |_| panic!("premature output")),
            Err(SourceError::Integrity)
        );
        assert_eq!(reader.next_reference(), Err(SourceError::Integrity));
    }
}

fn id(byte: u8) -> [u8; 16] {
    let mut id = [byte; 16];
    id[6] = 0x40;
    id[8] = 0x80;
    id
}

#[test]
fn digest_verifier_rejects_wrong_extent_before_hash() {
    let mut origin = DigestEnrollment::new(1, [id(1); 5], id(2)).unwrap();
    let mut page = origin.seal_chunk(&[9]).unwrap().page.unwrap();
    let manifest = origin.finish().unwrap();
    let verified = manifest.verify_page(0, &page).unwrap();
    assert!(verified.expectation(0).unwrap().final_chunk());
    assert!(verified.expectation(1).is_none());

    page[92] ^= 1;
    assert_eq!(
        manifest.verify_page(0, &page).err(),
        Some(PageError::HashMismatch)
    );
    page.extend_from_slice(&[0; 32]);
    assert_eq!(
        manifest.verify_page(0, &page).err(),
        Some(PageError::InvalidLength)
    );
}

#[test]
fn envelope_layout_and_enrollment_keep_distinct_bounds() {
    let size = PLAIN_CHUNK_SIZE + 1;
    let geometry = DigestGeometry::new(size).unwrap();
    let spans = plan_articles(size, 1).unwrap();
    let tail = plan_range(size, size - 1, size - 1).unwrap();
    assert_eq!(tail.ciphertext_start, spans[1].ciphertext_offset);
    assert_eq!(tail.ciphertext_end, geometry.sealed_len());
    assert_eq!(ciphertext_size(size), Some(geometry.sealed_len()));
    assert!(plan_range(u64::MAX, 0, 0).is_none());
    assert!(plan_articles(u64::MAX, u64::MAX).is_none());
    assert!(DigestGeometry::new(MAX_NEW_PLAINTEXT + 1).is_none());
    assert!(OriginSealer::new(MAX_NEW_PLAINTEXT + 1).is_err());

    let mut origin = DigestEnrollment::new(1, [id(1); 5], id(2)).unwrap();
    let context = EnvelopeContext {
        identities: [id(1); 6],
        plaintext_len: 1,
        header: origin.header().to_bytes(),
    };
    let root = [3; 32];
    let record = wrap_scheme2(&root, &context, origin.key().expose_secret()).unwrap();
    origin.seal_chunk(&[9]).unwrap();
    let manifest = origin.finish().unwrap();
    assert_eq!(
        verify_enrolled_scheme2(&root, &context, &record, &record, &manifest),
        Ok(())
    );
    assert_eq!(
        verify_enrolled_scheme2(&root, &context, &record, &record[..229], &manifest),
        Err(EnrolledEnvelopeError::Mismatch)
    );
    let mut extended = record.to_vec();
    extended.push(0);
    assert_eq!(
        verify_enrolled_scheme2(&root, &context, &record, &extended, &manifest),
        Err(EnrolledEnvelopeError::Mismatch)
    );
    let mut wrong_header = context;
    wrong_header.header[0] ^= 1;
    assert_eq!(
        verify_enrolled_scheme2(&root, &wrong_header, &record, &record, &manifest),
        Err(EnrolledEnvelopeError::Mismatch)
    );

    let mut fenced = DigestEnrollment::new(size, [id(1); 5], id(2)).unwrap();
    assert!(fenced.seal_chunk(&[9]).is_err());
    assert!(fenced
        .seal_chunk(&vec![9; PLAIN_CHUNK_SIZE as usize])
        .is_err());
    assert!(fenced.finish().is_err());
}
