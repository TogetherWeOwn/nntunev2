//! Synthetic encrypt-on-write streams only; no keys or article references from
//! production, no fixture files, no transport/provider diagnostics.

use nntune_core::content_verify::{ContentVerifier, VerifyError};
use nntune_core::decrypt::rclone::{CIPHER_BLOCK_SIZE, HEADER_SIZE, PLAIN_BLOCK_SIZE};
use nntune_core::encrypt::Encryptor;
use sha2::{Digest, Sha256};

struct Input {
    plaintext: Vec<u8>,
    key: [u8; 32],
    nonce: [u8; 24],
    sealed: Vec<u8>,
}

fn input(size: usize) -> Input {
    let key: [u8; 32] = Sha256::digest(b"nntune complete-read synthetic file key").into();
    let nonce_hash = Sha256::digest(b"nntune complete-read synthetic nonce");
    let mut nonce = [0u8; 24];
    nonce.copy_from_slice(&nonce_hash[..24]);
    let plaintext: Vec<u8> = (0..size)
        .map(|n| (n.wrapping_mul(37) % 251) as u8)
        .collect();
    let mut writer = Encryptor::new(key, nonce);
    let mut sealed = writer.header().to_vec();
    sealed.extend(writer.push(&plaintext).unwrap());
    sealed.extend(writer.finish().unwrap());
    Input {
        plaintext,
        key,
        nonce,
        sealed,
    }
}

impl Input {
    fn verifier(&self) -> ContentVerifier {
        ContentVerifier::new(
            &self.key,
            &self.nonce,
            self.plaintext.len() as u64,
            &Sha256::digest(&self.plaintext).into(),
        )
        .unwrap()
    }
}

#[test]
fn complete_stream_authenticates_exact_sizes_with_arbitrary_fragmentation() {
    for size in [1, 65_535, 65_536, 65_537, 196_741] {
        let data = input(size);
        for fragment in [1, 7, 31, 65_552, 200_000] {
            let mut verifier = data.verifier();
            verifier.push(&[]).unwrap();
            for chunk in data.sealed.chunks(fragment) {
                verifier.push(chunk).unwrap();
            }
            verifier.push(&[]).unwrap();
            let result = verifier.finish().unwrap();
            assert_eq!(result.plaintext_size(), size as u64);
            let expected: [u8; 32] = Sha256::digest(&data.plaintext).into();
            assert_eq!(result.sha256(), &expected);
        }
    }
}

#[test]
fn truncated_header_blocks_and_short_tail_never_produce_a_result() {
    let data = input(2 * PLAIN_BLOCK_SIZE as usize + 13);
    for end in [0, 1, 31, 32, 33, 48, 65_583, 65_584, data.sealed.len() - 1] {
        let mut verifier = data.verifier();
        verifier.push(&data.sealed[..end]).unwrap();
        assert_eq!(verifier.finish().unwrap_err(), VerifyError::Incomplete);
    }
}

#[test]
fn tampered_tag_or_payload_in_every_block_fails_authentication_and_stays_failed() {
    let data = input(2 * PLAIN_BLOCK_SIZE as usize + 13);
    for block in 0..3 {
        for within in [0, 16] {
            let mut sealed = data.sealed.clone();
            sealed[HEADER_SIZE as usize + block * CIPHER_BLOCK_SIZE as usize + within] ^= 1;
            let mut verifier = data.verifier();
            assert_eq!(
                verifier.push(&sealed),
                Err(VerifyError::AuthenticationFailed)
            );
            // A fresh full stream after an error cannot rehabilitate this read.
            assert_eq!(
                verifier.push(&data.sealed),
                Err(VerifyError::AuthenticationFailed)
            );
            assert_eq!(
                verifier.finish().unwrap_err(),
                VerifyError::AuthenticationFailed
            );
        }
    }
}

#[test]
fn wrong_key_fails_without_returning_plaintext_or_diagnostics() {
    let data = input(65_537);
    let wrong = Sha256::digest(b"nntune different synthetic file key").into();
    let mut verifier = ContentVerifier::new(
        &wrong,
        &data.nonce,
        data.plaintext.len() as u64,
        &Sha256::digest(&data.plaintext).into(),
    )
    .unwrap();
    assert_eq!(
        verifier.push(&data.sealed),
        Err(VerifyError::AuthenticationFailed)
    );
    assert_eq!(
        format!("{:?}", verifier.finish().unwrap_err()),
        "AuthenticationFailed"
    );
}

#[test]
fn header_magic_and_nonce_are_bound_to_the_stored_expectation() {
    let data = input(101);
    for index in [0, 7, 8, 31] {
        let mut sealed = data.sealed.clone();
        sealed[index] ^= 1;
        let mut verifier = data.verifier();
        assert_eq!(verifier.push(&sealed), Err(VerifyError::IntegrityFailed));
        assert_eq!(verifier.finish().unwrap_err(), VerifyError::IntegrityFailed);
    }
    let mut different_nonce = data.nonce;
    different_nonce[0] ^= 1;
    let mut verifier = ContentVerifier::new(
        &data.key,
        &different_nonce,
        data.plaintext.len() as u64,
        &Sha256::digest(&data.plaintext).into(),
    )
    .unwrap();
    assert_eq!(
        verifier.push(&data.sealed),
        Err(VerifyError::IntegrityFailed)
    );
}

#[test]
fn valid_blocks_with_the_wrong_plaintext_digest_are_not_intact() {
    let data = input(65_539);
    let other_digest = Sha256::digest(b"different complete plaintext").into();
    let mut verifier = ContentVerifier::new(
        &data.key,
        &data.nonce,
        data.plaintext.len() as u64,
        &other_digest,
    )
    .unwrap();
    verifier.push(&data.sealed).unwrap();
    assert_eq!(verifier.finish().unwrap_err(), VerifyError::IntegrityFailed);
}

#[test]
fn extra_ciphertext_is_rejected_even_after_all_expected_bytes_authenticated() {
    let data = input(65_537);
    let mut verifier = data.verifier();
    verifier.push(&data.sealed).unwrap();
    assert_eq!(verifier.push(&[0]), Err(VerifyError::IntegrityFailed));
    assert_eq!(verifier.push(&[]), Err(VerifyError::IntegrityFailed));
    assert_eq!(verifier.finish().unwrap_err(), VerifyError::IntegrityFailed);
    let mut verifier = data.verifier();
    let mut oversized = data.sealed.clone();
    oversized.push(0);
    assert_eq!(verifier.push(&oversized), Err(VerifyError::IntegrityFailed));
}

#[test]
fn reordered_or_repeated_blocks_fail_position_authentication() {
    let data = input(2 * PLAIN_BLOCK_SIZE as usize);
    let a = HEADER_SIZE as usize;
    let b = a + CIPHER_BLOCK_SIZE as usize;
    for repeated in [false, true] {
        let mut changed = data.sealed[..a].to_vec();
        changed.extend_from_slice(&data.sealed[b..]);
        changed.extend_from_slice(if repeated {
            &data.sealed[b..]
        } else {
            &data.sealed[a..b]
        });
        let mut verifier = data.verifier();
        assert_eq!(
            verifier.push(&changed),
            Err(VerifyError::AuthenticationFailed)
        );
    }
}

#[test]
fn authenticated_but_different_same_key_nonce_stream_is_caught_by_full_digest() {
    let data = input(65_539);
    let mut other = data.plaintext.clone();
    other[65_537] ^= 1;
    let mut writer = Encryptor::new(data.key, data.nonce);
    let mut sealed = writer.header().to_vec();
    sealed.extend(writer.push(&other).unwrap());
    sealed.extend(writer.finish().unwrap());
    let mut verifier = data.verifier();
    verifier.push(&sealed).unwrap();
    assert_eq!(verifier.finish().unwrap_err(), VerifyError::IntegrityFailed);
}

#[test]
fn declared_plaintext_size_cannot_hide_a_short_or_long_stream() {
    let data = input(65_539);
    for size in [1, 65_536, 65_538, 65_540, 131_072] {
        let mut verifier = ContentVerifier::new(
            &data.key,
            &data.nonce,
            size,
            &Sha256::digest(&data.plaintext).into(),
        )
        .unwrap();
        // Whichever check rejects first, no mismatched size yields a result.
        let _ = verifier.push(&data.sealed);
        assert!(verifier.finish().is_err());
    }
}

#[test]
fn zero_and_overflowing_geometry_fail_before_a_verifier_is_created() {
    let data = input(1);
    for size in [0, u64::MAX] {
        assert_eq!(
            ContentVerifier::new(
                &data.key,
                &data.nonce,
                size,
                &Sha256::digest(&data.plaintext).into(),
            )
            .unwrap_err(),
            VerifyError::InvalidInput
        );
    }
}

#[test]
fn sha256_state_and_partial_plaintext_buffer_have_drop_time_wiping() {
    fn requires_zeroizing_drop<T: zeroize::ZeroizeOnDrop>() {}
    requires_zeroizing_drop::<Sha256>();
}

#[test]
fn debug_redacts_in_progress_and_completed_content() {
    let data = input(101);
    let mut verifier = data.verifier();
    verifier.push(&data.sealed[..37]).unwrap();
    assert_eq!(format!("{verifier:?}"), "ContentVerifier([REDACTED])");
    verifier.push(&data.sealed[37..]).unwrap();
    assert_eq!(
        format!("{:?}", verifier.finish().unwrap()),
        "VerifiedContent([REDACTED])"
    );
}
