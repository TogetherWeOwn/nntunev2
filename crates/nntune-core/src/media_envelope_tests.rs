use super::*;

fn decode(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap())
        .collect()
}

fn context(root: u8, plaintext_len: u64) -> EnvelopeContext {
    let mut identities = [[0; 16]; 6];
    for (id, value) in identities
        .iter_mut()
        .zip([root, 0x22, 0x33, 0x44, 0x55, 0x66])
    {
        *id = [value; 16];
        id[6] = 0x40 | (value & 0x0f);
        id[8] = 0x80 | (value & 0x0f);
    }
    let header = decode(concat!(
        "a0a1a2a3a4a5a6a7a8a9aaabacadaeaf",
        "2d2806b6af62d7b84992bad446eef0e88cf20773d81037ad892c9c57db849500"
    ));
    EnvelopeContext {
        identities,
        plaintext_len,
        header: header.try_into().unwrap(),
    }
}

// Public envelope fields from the approved synthetic scheme-2 vectors; no keys.
fn vectors() -> Vec<Vec<u8>> {
    include_str!("media_envelope_vectors.txt")
        .lines()
        .map(decode)
        .collect()
}

#[test]
fn approved_envelope_vectors_match_independent_context() {
    let lengths = [0, 1, 65535, 65536, 65537, 65537, 65537, 65537, 65537, 65537];
    let roots = [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x77, 0x88];
    for ((record, len), root) in vectors().iter().zip(lengths).zip(roots) {
        assert!(matches_scheme2(record, &context(root, len)));
    }
}

#[test]
fn synthetic_vectors_authenticate_and_wrap() {
    let records = vectors();
    let lengths = [0, 1, 65535, 65536, 65537, 65537, 65537, 65537, 65537, 65537];
    let versions = [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x77, 0x88];
    let key: [u8; 32] = std::array::from_fn(|i| i as u8);
    for (i, record) in records.iter().enumerate() {
        let root: [u8; 32] = std::array::from_fn(|j| (if i == 8 { 0x60 } else { 0x40 }) + j as u8);
        let context = context(versions[i], lengths[i]);
        assert_eq!(&*unwrap_scheme2(&root, &context, record).unwrap(), &key);
        // Duplicate records model persisted no-op/retry/restore, not fresh encryption.
        if !(5..=7).contains(&i) {
            assert_eq!(
                wrap_scheme2(&root, &context, &key).unwrap().as_slice(),
                record
            );
        } else {
            assert_eq!(record, &records[4]);
        }
    }
}

#[test]
fn authentication_rejects_tampering_and_wrong_trusted_inputs() {
    let record = &vectors()[0];
    let root: [u8; 32] = std::array::from_fn(|i| 0x40 + i as u8);
    let expected = context(0x11, 0);
    for offset in 0..230 {
        let mut altered = record.clone();
        altered[offset] ^= 1;
        let error = unwrap_scheme2(&root, &expected, &altered).unwrap_err();
        assert_eq!(
            error,
            if offset < 182 {
                EnvelopeError::InvalidContext
            } else {
                EnvelopeError::Authentication
            },
            "offset {offset}"
        );
    }
    for len in 0..230 {
        assert!(unwrap_scheme2(&root, &expected, &record[..len]).is_err());
    }
    let mut extended = record.clone();
    extended.push(0);
    assert!(unwrap_scheme2(&root, &expected, &extended).is_err());
    assert!(unwrap_scheme2(&[0; 32], &expected, record).is_err());
    assert!(unwrap_scheme2(&root, &context(0x77, 0), record).is_err());
    assert!(unwrap_scheme2(&root, &context(0x11, 1), record).is_err());
    let mut different_header = context(0x11, 0);
    different_header.header[0] ^= 1;
    assert!(unwrap_scheme2(&root, &different_header, record).is_err());
}

#[test]
fn malformed_or_untrusted_prefixes_reject_without_authentication_claims() {
    let record = &vectors()[0];
    let expected = context(0x11, 0);
    for len in 0..230 {
        assert!(!matches_scheme2(&record[..len], &expected));
    }
    let mut extended = record.clone();
    extended.push(0);
    assert!(!matches_scheme2(&extended, &expected));
    for offset in 0..182 {
        let mut altered = record.clone();
        altered[offset] ^= 1;
        assert!(!matches_scheme2(&altered, &expected), "offset {offset}");
    }
    for i in 0..6 {
        for offset in [6, 8] {
            let mut invalid = context(0x11, 0);
            invalid.identities[i][offset] = 0;
            let mut altered = record.clone();
            altered[10 + i * 16 + offset] = 0;
            assert!(!matches_scheme2(&altered, &invalid));
        }
    }
    assert!(!matches_scheme2(record, &context(0x11, u64::MAX)));
    let mut altered = record.clone();
    altered[182] ^= 1;
    altered[229] ^= 1;
    assert!(matches_scheme2(&altered, &expected)); // Structural, not AEAD validation.
}
