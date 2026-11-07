//! Synthetic media only. The engine owns crypto; these checks pin the NNTune
//! profile and article coordinates at the shim/preparation integration boundary.
use nntune_core::{
    media::{self, ContentIdentity, FileKey, KeyMode, Profile, Sealer, VerifiedHeader},
    media_layout::plan_articles,
    rng::SystemRng,
};

#[test]
fn article_spans_and_ranged_reads_match_the_successor_wire_format() {
    for size in [0, 1, 65_535, 65_536, 65_537, 11 * 65_536 + 1] {
        let plain: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        let key = FileKey::generate(&mut SystemRng).unwrap();
        let sealer = Sealer::random(&Profile::nntune(), &key, &mut SystemRng).unwrap();
        let header = sealer.header();
        let sealed = sealer.seal_to_vec(&plain).unwrap();
        assert_eq!(sealed.len() as u64, media::ciphertext_size(size).unwrap());
        let verified = VerifiedHeader::verify_record(
            &Profile::nntune(),
            KeyMode::Random.as_byte(),
            &key,
            &header,
        )
        .unwrap();
        let reader = verified.reader(size).unwrap();
        reader.check_wire_header(&sealed[..48]).unwrap();
        assert_eq!(
            &reader.verify_all(sealed.as_slice()).unwrap(),
            ContentIdentity::of(&plain).digest()
        );
        for policy in [1, 11] {
            for span in plan_articles(size, policy).unwrap() {
                let mut out = vec![0; (span.plaintext_end - span.plaintext_start) as usize];
                assert_eq!(
                    reader
                        .read_at(sealed.as_slice(), &mut out, span.plaintext_start)
                        .unwrap(),
                    out.len()
                );
                assert_eq!(
                    out,
                    plain[span.plaintext_start as usize..span.plaintext_end as usize]
                );
                assert!(span.ciphertext_offset + span.ciphertext_len <= sealed.len() as u64);
            }
        }
        if size > 65_536 {
            let mut crossing = [0; 3];
            let n = reader
                .read_at(sealed.as_slice(), &mut crossing, 65_535)
                .unwrap();
            assert_eq!(&crossing[..n], &plain[65_535..65_535 + n]);
        }
    }
}

#[test]
fn complete_read_rejects_tampering_truncation_and_extra_bytes() {
    let plain = vec![7; 65_537];
    let key = FileKey::generate(&mut SystemRng).unwrap();
    let sealer = Sealer::random(&Profile::nntune(), &key, &mut SystemRng).unwrap();
    let header = sealer.header();
    let sealed = sealer.seal_to_vec(&plain).unwrap();
    let verified =
        VerifiedHeader::verify(&Profile::nntune(), KeyMode::Random, &key, &header).unwrap();
    let reader = verified.reader(plain.len() as u64).unwrap();
    for offset in [0, 16, 47, 48, sealed.len() - 1] {
        let mut changed = sealed.clone();
        changed[offset] ^= 1;
        assert!(reader.verify_all(changed.as_slice()).is_err());
    }
    assert!(reader.verify_all(&sealed[..sealed.len() - 1]).is_err());
    let mut extra = sealed;
    extra.push(0);
    assert!(reader.verify_all(extra.as_slice()).is_err());
    let wrong = FileKey::generate(&mut SystemRng).unwrap();
    assert!(VerifiedHeader::verify(&Profile::nntune(), KeyMode::Random, &wrong, &header).is_err());
}

#[test]
fn legacy_nonce_shape_is_not_a_media_v1_header() {
    assert_eq!(media::HEADER_SIZE, 48);
    assert!(media::Header::from_slice(&[0; 24]).is_err());
    assert!(media::Header::from_slice(&[0; 32]).is_err());
    let key = FileKey::generate(&mut SystemRng).unwrap();
    let untrusted = media::Header::from_slice(&[0; 48]).unwrap();
    assert!(VerifiedHeader::verify_record(&Profile::nntune(), 255, &key, &untrusted).is_err());
    assert!(VerifiedHeader::verify(&Profile::nntune(), KeyMode::Random, &key, &untrusted).is_err());
}
