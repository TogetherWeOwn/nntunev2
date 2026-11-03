//! Encrypt-then-decrypt acceptance: the [`nntune_core::encrypt`] writer
//! seals streams that the existing [`nntune_core::decrypt::rclone`] reader
//! recovers exactly, at block and range boundaries.
//!
//! All keys and nonces are synthetic SHA-256 test vectors (fixed labels
//! plus a tag); nothing here is production key material, and nothing is
//! logged or persisted.

use nntune_core::decrypt::rclone::{self, CIPHER_BLOCK_SIZE, HEADER_SIZE, PLAIN_BLOCK_SIZE};
use nntune_core::encrypt::{ciphertext_len, plan_articles, Encryptor};

fn test_key(tag: u64) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"nntune-core integration encrypt test file key v1");
    h.update(tag.to_le_bytes());
    h.finalize().into()
}

fn test_nonce(tag: u64) -> [u8; 24] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"nntune-core integration encrypt test nonce v1");
    h.update(tag.to_le_bytes());
    let digest = h.finalize();
    let mut nonce = [0u8; 24];
    nonce.copy_from_slice(&digest[..24]);
    nonce
}

fn derived_of(key: &[u8; 32]) -> [u8; 80] {
    let mut derived = [0u8; 80];
    derived[..32].copy_from_slice(key);
    derived
}

/// Deterministic pseudo-random bytes (xorshift64*): exercises tag
/// authentication over non-repetitive data without fixture files.
fn pattern(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            (state.wrapping_mul(0x2545F4914F6CDD1D) >> 56) as u8
        })
        .collect()
}

fn seal_stream(plaintext: &[u8], tag: u64, push_sizes: &[usize]) -> (Vec<u8>, [u8; 32]) {
    let key = test_key(tag);
    let mut enc = Encryptor::new(key, test_nonce(tag));
    let mut sealed = enc.header().to_vec();
    let mut rest = plaintext;
    let mut pick = 0;
    while !rest.is_empty() {
        let take = push_sizes[pick % push_sizes.len()].min(rest.len());
        pick += 1;
        sealed.extend_from_slice(&enc.push(&rest[..take]).unwrap());
        rest = &rest[take..];
    }
    sealed.extend_from_slice(&enc.finish().unwrap());
    (sealed, key)
}

fn decrypt_all(sealed: &Vec<u8>, key: &[u8; 32], size: u64) -> Vec<u8> {
    let mut out = vec![0u8; size as usize];
    let mut off = 0u64;
    while off < size {
        let mut window = vec![0u8; 100_000];
        let n = rclone::read_at(sealed, &derived_of(key), size, &mut window, off).unwrap();
        assert!(n > 0, "reader stalled at offset {off}");
        out[off as usize..off as usize + n].copy_from_slice(&window[..n]);
        off += n as u64;
    }
    out
}

#[test]
fn writer_output_matches_reader_at_scale() {
    // 200,003 bytes: three full blocks plus a 131-byte tail, pushed in
    // uneven caller-sized chunks (1 byte .. 90,000 bytes).
    let size = 3 * PLAIN_BLOCK_SIZE + 131;
    let plaintext = pattern(size as usize, 0x123456789ABCDEF);
    let (sealed, key) = seal_stream(&plaintext, 11, &[1, 7, 65_535, 65_537, 90_000]);
    assert_eq!(sealed.len() as u64, ciphertext_len(size).unwrap());
    assert_eq!(decrypt_all(&sealed, &key, size), plaintext);
}

#[test]
fn article_plan_covers_sealed_stream_without_gaps() {
    // Geometry the poster relies on: concatenating per-article ciphertext
    // windows behind the header reproduces the sealed stream exactly.
    let size = 11 * PLAIN_BLOCK_SIZE + 999;
    let plaintext = pattern(size as usize, 77);
    let (sealed, key) = seal_stream(&plaintext, 12, &[40_000]);
    let spans = plan_articles(size, 11).unwrap();
    assert_eq!(spans.len(), 2);
    let mut rebuilt = sealed[..HEADER_SIZE as usize].to_vec();
    let mut plain_cursor = 0u64;
    for span in &spans {
        assert_eq!(span.plaintext_start, plain_cursor);
        plain_cursor = span.plaintext_end;
        rebuilt.extend_from_slice(
            &sealed[span.ciphertext_offset as usize
                ..span.ciphertext_offset as usize + span.ciphertext_len as usize],
        );
    }
    assert_eq!(plain_cursor, size);
    assert_eq!(rebuilt, sealed);
    assert_eq!(decrypt_all(&sealed, &key, size), plaintext);
}

#[test]
fn range_reads_recover_article_windows() {
    // Simulates play fetching one article window per range: every span of
    // an 11-block article plan decrypts to its plaintext range.
    let size = 25 * PLAIN_BLOCK_SIZE + 5;
    let plaintext = pattern(size as usize, 2024);
    let (sealed, key) = seal_stream(&plaintext, 13, &[65_536]);
    let spans = plan_articles(size, 11).unwrap();
    assert_eq!(spans.len(), 3);
    for span in &spans {
        let cipher_start = span.ciphertext_offset as usize;
        let cipher_end = cipher_start + span.ciphertext_len as usize;
        // Ciphertext offsets advance by whole sealed blocks from the header.
        assert_eq!((cipher_start as u64 - HEADER_SIZE) % CIPHER_BLOCK_SIZE, 0);
        let window = &sealed[cipher_start..cipher_end];
        // Decrypt the window through the ranged reader: the first block of
        // the window starts at a block boundary, so a block-aligned read
        // of the span's plaintext range must match.
        let block_index = (cipher_start as u64 - HEADER_SIZE) / CIPHER_BLOCK_SIZE;
        let plain_off = block_index * PLAIN_BLOCK_SIZE;
        let plain_len = (span.plaintext_end - span.plaintext_start) as usize;
        let mut dst = vec![0u8; plain_len];
        let n = rclone::read_at(&sealed, &derived_of(&key), size, &mut dst, plain_off).unwrap();
        assert_eq!(n, plain_len);
        assert_eq!(
            &dst,
            &plaintext[plain_off as usize..plain_off as usize + plain_len]
        );
        assert_eq!(window.len() as u64, span.ciphertext_len);
    }
}
