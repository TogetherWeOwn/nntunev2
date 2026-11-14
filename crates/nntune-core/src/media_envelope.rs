//! Scheme-2 envelope structure only; success does not authenticate any bytes.
//!
//! The caller must obtain the expected fields from independently trusted state
//! and separately enforce key purpose, lifecycle and immutable locator policy.

use crate::media::ciphertext_size;

/// Public context from trusted state, not from the envelope being checked.
pub struct EnvelopeContext {
    /// Root version, environment, library, object, generation and provider version.
    pub identities: [[u8; 16]; 6],
    pub plaintext_len: u64,
    pub header: [u8; 48],
}

/// Check the exact scheme-2 encoding and trusted prefix, without decrypting.
/// This does not check the wrapped key, tag, header commitment or ciphertext.
pub fn matches_scheme2(record: &[u8], expected: &EnvelopeContext) -> bool {
    if record.len() != 230 || &record[..10] != b"NNTMED01\0\x02" {
        return false;
    }
    for (i, id) in expected.identities.iter().enumerate() {
        if id[6] >> 4 != 4 || id[8] >> 6 != 2 || record[10 + 16 * i..26 + 16 * i] != id[..] {
            return false;
        }
    }
    let Some(encoded_len) = ciphertext_size(expected.plaintext_len) else {
        return false;
    };
    record[106..114] == expected.plaintext_len.to_be_bytes()
        && record[114..122] == encoded_len.to_be_bytes()
        && record[122..170] == expected.header
        && record[170..182] == [0; 12]
}

#[cfg(test)]
#[path = "media_envelope_tests.rs"]
mod tests;
