use nntune_core::{
    digest_enrollment::DigestEnrollment,
    media_envelope::{
        matches_scheme2, unwrap_scheme2, verify_enrolled_scheme2, wrap_scheme2,
        EnrolledEnvelopeError, EnvelopeContext, EnvelopeError,
    },
};

#[test]
fn structural_match_cannot_substitute_for_retained_bytes_or_aead() {
    let mut identity = [1; 16];
    identity[6] = 0x40;
    identity[8] = 0x80;
    let mut origin = DigestEnrollment::new(1, [identity; 5], identity).unwrap();
    let context = EnvelopeContext {
        identities: [identity; 6],
        plaintext_len: 1,
        header: origin.header().to_bytes(),
    };
    let root = [3; 32];
    let retained = wrap_scheme2(&root, &context, origin.key().expose_secret()).unwrap();
    origin.seal_chunk(&[7]).unwrap();
    let manifest = origin.finish().unwrap();
    assert_eq!(
        verify_enrolled_scheme2(&root, &context, &retained, &retained, &manifest),
        Ok(())
    );

    let mut bad_tag = retained;
    bad_tag[229] ^= 1;
    assert!(matches_scheme2(&bad_tag, &context));
    assert_eq!(
        verify_enrolled_scheme2(&root, &context, &retained, &bad_tag, &manifest),
        Err(EnrolledEnvelopeError::Mismatch)
    );
    assert_eq!(
        verify_enrolled_scheme2(&root, &context, &bad_tag, &bad_tag, &manifest),
        Err(EnrolledEnvelopeError::Envelope(
            EnvelopeError::Authentication
        ))
    );

    let mut bad_nonce = retained;
    bad_nonce[170] = 1;
    assert!(!matches_scheme2(&bad_nonce, &context));
    assert_eq!(
        unwrap_scheme2(&root, &context, &bad_nonce).unwrap_err(),
        EnvelopeError::InvalidContext
    );
}
