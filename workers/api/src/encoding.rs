//! Base64url helpers shared by the HTTP/WebAuthn layers (no padding).
//! Existing modules keep their private copies; new code uses these.

pub fn b64url_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((n >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 63) as usize] as char);
        }
    }
    out
}

fn val(c: u8, flexible: bool) -> Result<u32, ()> {
    match c {
        b'A'..=b'Z' => Ok((c - b'A') as u32),
        b'a'..=b'z' => Ok((c - b'a' + 26) as u32),
        b'0'..=b'9' => Ok((c - b'0' + 52) as u32),
        b'-' => Ok(62),
        b'_' => Ok(63),
        b'+' if flexible => Ok(62),
        b'/' if flexible => Ok(63),
        _ => Err(()),
    }
}

fn decode(s: &str, flexible: bool) -> Result<Vec<u8>, ()> {
    let trimmed: Vec<u8> = if flexible {
        s.bytes()
            .filter(|&b| b != b'=' && !b.is_ascii_whitespace())
            .collect()
    } else {
        s.bytes().collect()
    };
    if trimmed.len() % 4 == 1 {
        return Err(());
    }
    let mut out = Vec::with_capacity(trimmed.len() / 4 * 3);
    for chunk in trimmed.chunks(4) {
        let mut n = 0u32;
        let mut present = 0usize;
        for &c in chunk {
            n = (n << 6) | val(c, flexible)?;
            present += 1;
        }
        n <<= 6 * (4 - present);
        out.push(((n >> 16) & 0xFF) as u8);
        if present > 2 {
            out.push(((n >> 8) & 0xFF) as u8);
        }
        if present > 3 {
            out.push((n & 0xFF) as u8);
        }
    }
    // Re-encoding must round-trip: rejects non-canonical trailing bits
    // (e.g. a final char carrying ignored bits that alias another value).
    if b64url_encode(&out).as_bytes() != trimmed_flexible_normalized(&trimmed, flexible) {
        return Err(());
    }
    Ok(out)
}

fn trimmed_flexible_normalized(bytes: &[u8], flexible: bool) -> Vec<u8> {
    if !flexible {
        return bytes.to_vec();
    }
    bytes
        .iter()
        .map(|&b| match b {
            b'+' => b'-',
            b'/' => b'_',
            other => other,
        })
        .collect()
}

/// Strict unpadded base64url (WebAuthn/JSON wire format). The unit error
/// is deliberate: callers map every decode failure to one generic error.
#[allow(clippy::result_unit_err)]
pub fn b64url_decode(s: &str) -> Result<Vec<u8>, ()> {
    decode(s, false)
}

/// Base64 accepting either alphabet and optional padding/whitespace.
/// Only for operator-provisioned secrets (KEK/session secret), never
/// for client input.
#[allow(clippy::result_unit_err)]
pub fn b64_decode_flexible(s: &str) -> Result<Vec<u8>, ()> {
    decode(s, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for data in [&b""[..], b"f", b"fo", b"foo", b"foob", b"\xff\xfe\xfd\x00"] {
            assert_eq!(b64url_decode(&b64url_encode(data)).unwrap(), data);
        }
    }

    #[test]
    fn rejects_noncanonical_and_padding() {
        assert!(b64url_decode("Zg==").is_err());
        assert!(b64url_decode("Zh").is_err()); // trailing bits set: aliases "Zg"
        assert!(b64url_decode("a").is_err());
        assert!(b64url_decode("a b").is_err());
    }

    #[test]
    fn flexible_accepts_standard_alphabet_with_padding() {
        assert_eq!(b64_decode_flexible("Zg==").unwrap(), b"f");
        assert_eq!(b64_decode_flexible("+/8=").unwrap(), [0xfb, 0xff]);
        assert_eq!(b64_decode_flexible("-_8").unwrap(), [0xfb, 0xff]);
    }
}
