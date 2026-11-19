use nntune_core::digest_enrollment::DigestEnrollment;
use nntune_core::digest_range::{DigestRange, RangeError};
use nntune_core::media::FileKey;
use nntune_core::media_envelope::{
    unwrap_scheme2, verify_enrolled_scheme2, wrap_scheme2, EnrolledEnvelopeError, EnvelopeContext,
    EnvelopeError,
};

fn id(n: u8) -> [u8; 16] {
    let mut id = [n; 16];
    id[6] = 0x40;
    id[8] = 0x80;
    id
}

#[test]
fn enrolled_key_envelope_and_range_compose() {
    let plaintext: Vec<u8> = (0..65_600).map(|i| (i % 251) as u8).collect();
    let identities = [id(2), id(3), id(4), id(5), id(6)];
    let mut enrollment = DigestEnrollment::new(plaintext.len() as u64, identities, id(7)).unwrap();
    let header = enrollment.header().to_bytes();
    let context = EnvelopeContext {
        identities: [id(1), id(2), id(3), id(4), id(5), id(6)],
        plaintext_len: plaintext.len() as u64,
        header,
    };
    let root = [9; 32];
    let envelope = wrap_scheme2(&root, &context, enrollment.key().expose_secret()).unwrap();
    let mut chunks = Vec::new();
    let mut pages = Vec::new();
    for part in plaintext.chunks(65_536) {
        let output = enrollment.seal_chunk(part).unwrap();
        chunks.push(output.bytes);
        if let Some(page) = output.page {
            pages.push(page);
        }
    }
    let manifest = enrollment.finish().ok().unwrap();
    assert_eq!((chunks.len(), pages.len()), (2, 1));

    assert_eq!(
        verify_enrolled_scheme2(&root, &context, &envelope, &envelope, &manifest),
        Ok(())
    );
    let unwrapped = unwrap_scheme2(&root, &context, &envelope).unwrap();
    let key = FileKey::from_bytes(*unwrapped);
    let mut range = DigestRange::new(&manifest, &key, 65_530, 65_545).unwrap();
    assert_eq!(range.next_indices(), Some((0, 0)));
    let first = range
        .open_next(0, &pages[0], chunks[0].clone(), Some(&header))
        .unwrap();
    assert_eq!(range.next_indices(), Some((1, 0)));
    let second = range
        .open_next(1, &pages[0], chunks[1].clone(), None)
        .unwrap();
    assert_eq!(
        [first.as_slice(), second.as_slice()].concat(),
        plaintext[65_530..65_545]
    );
    assert_eq!(range.next_indices(), None);

    let wrong_context = EnvelopeContext {
        identities: [id(1), id(2), id(8), id(4), id(5), id(6)],
        plaintext_len: plaintext.len() as u64,
        header,
    };
    assert!(matches!(
        unwrap_scheme2(&root, &wrong_context, &envelope),
        Err(EnvelopeError::InvalidContext)
    ));
    assert!(matches!(
        unwrap_scheme2(&[8; 32], &context, &envelope),
        Err(EnvelopeError::Authentication)
    ));
    let mut tampered_envelope = envelope;
    tampered_envelope[200] ^= 1;
    assert!(matches!(
        unwrap_scheme2(&root, &context, &tampered_envelope),
        Err(EnvelopeError::Authentication)
    ));

    assert_eq!(
        verify_enrolled_scheme2(&root, &context, &envelope, &tampered_envelope, &manifest),
        Err(EnrolledEnvelopeError::Mismatch)
    );
    assert_eq!(
        verify_enrolled_scheme2(
            &root,
            &context,
            &tampered_envelope,
            &tampered_envelope,
            &manifest,
        ),
        Err(EnrolledEnvelopeError::Envelope(
            EnvelopeError::Authentication
        ))
    );
    assert_eq!(
        verify_enrolled_scheme2(&root, &wrong_context, &envelope, &envelope, &manifest),
        Err(EnrolledEnvelopeError::Mismatch)
    );

    // A separately wrapped key authenticates as an envelope but does not
    // match the sealer's header commitment. Use a distinct test root/version.
    let wrong_key_context = EnvelopeContext {
        identities: [id(10), id(2), id(3), id(4), id(5), id(6)],
        plaintext_len: plaintext.len() as u64,
        header,
    };
    let wrong_key_root = [10; 32];
    let wrong_key_record = wrap_scheme2(&wrong_key_root, &wrong_key_context, &[3; 32]).unwrap();
    assert_eq!(
        verify_enrolled_scheme2(
            &wrong_key_root,
            &wrong_key_context,
            &wrong_key_record,
            &wrong_key_record,
            &manifest,
        ),
        Err(EnrolledEnvelopeError::Header)
    );

    let mut bad_page = pages[0].clone();
    bad_page[92] ^= 1;
    let mut range = DigestRange::new(&manifest, &key, 65_540, 65_545).unwrap();
    assert!(matches!(
        range.open_next(1, &bad_page, chunks[1].clone(), None),
        Err(RangeError::Page(_))
    ));
    assert_eq!(range.next_indices(), None);
    let mut bad_chunk = chunks[1].clone();
    bad_chunk[0] ^= 1;
    let mut range = DigestRange::new(&manifest, &key, 65_540, 65_545).unwrap();
    assert!(matches!(
        range.open_next(1, &pages[0], bad_chunk, None),
        Err(RangeError::DigestMismatch)
    ));
    assert_eq!(range.next_indices(), None);
}
