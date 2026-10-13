//! Minimal yEnc codec for the miss path: NNTP segments arrive yEnc-encoded;
//! the play path decodes them before decrypting.
//!
//! Everything is byte-based: encoded bodies routinely contain bytes ≥ 128,
//! which are not valid UTF-8. [`encode`] exists so tests (and the synthetic
//! NNTP fixture) can build byte-exact articles; the Worker only ever calls
//! [`decode`].

/// Why a yEnc body was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum YencError {
    Truncated,
    BadEscape,
    SizeMismatch { expected: u64, actual: usize },
    CrcMismatch { expected: u32, actual: u32 },
}

/// IEEE CRC-32 over `data` (bitwise; no table needed at this size).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for b in data {
        crc ^= *b as u32;
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB88320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

const WRAP_COL: usize = 128;

/// Encode `data` as a yEnc article naming `name` (CRLF line endings).
pub fn encode(data: &[u8], name: &str) -> Vec<u8> {
    let mut out = format!(
        "=ybegin line={WRAP_COL} size={} name={name}\r\n",
        data.len()
    )
    .into_bytes();
    let mut col = 0usize;
    for b in data {
        let c = b.wrapping_add(42);
        let width = if matches!(c, 0x00 | 0x0A | 0x0D | 0x3D) {
            2
        } else {
            1
        };
        if col + width > WRAP_COL {
            out.extend_from_slice(b"\r\n");
            col = 0;
        }
        if width == 2 {
            out.push(b'=');
            out.push(c.wrapping_add(64));
        } else {
            out.push(c);
        }
        col += width;
    }
    out.extend_from_slice(
        format!(
            "\r\n=yend size={} crc32={:08x}\r\n",
            data.len(),
            crc32(data)
        )
        .as_bytes(),
    );
    out
}

/// Decode a full yEnc article (headers, body, `=yend` trailer) to bytes,
/// verifying size and CRC-32.
pub fn decode(text: &[u8]) -> Result<Vec<u8>, YencError> {
    let mut out = Vec::new();
    let mut in_body = false;
    let mut seen_trailer = false;
    let mut expected_size: Option<u64> = None;
    let mut expected_crc: Option<u32> = None;
    for raw_line in text.split(|&b| b == b'\n') {
        let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        if line.starts_with(b"=ybegin") {
            in_body = true;
            continue;
        }
        if line.starts_with(b"=yend") {
            in_body = false;
            seen_trailer = true;
            let trailer = std::str::from_utf8(line).map_err(|_| YencError::Truncated)?;
            expected_size = tag_u64(trailer, "size");
            expected_crc = tag_hex_u32(trailer, "crc32");
            continue;
        }
        if !in_body || line.is_empty() {
            continue;
        }
        let mut i = 0;
        while i < line.len() {
            if line[i] == b'=' {
                i += 1;
                if i >= line.len() {
                    return Err(YencError::BadEscape);
                }
                out.push(line[i].wrapping_sub(64).wrapping_sub(42));
            } else {
                out.push(line[i].wrapping_sub(42));
            }
            i += 1;
        }
    }
    if !seen_trailer || expected_size.is_none() || expected_crc.is_none() {
        return Err(YencError::Truncated);
    }
    let (expected_size, expected_crc) = (expected_size.unwrap(), expected_crc.unwrap());
    if expected_size as usize != out.len() {
        return Err(YencError::SizeMismatch {
            expected: expected_size,
            actual: out.len(),
        });
    }
    let actual = crc32(&out);
    if actual != expected_crc {
        return Err(YencError::CrcMismatch {
            expected: expected_crc,
            actual,
        });
    }
    Ok(out)
}

fn tag_u64(trailer: &str, key: &str) -> Option<u64> {
    let prefix = format!("{key}=");
    trailer
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix(prefix.as_str())?.parse().ok())
}

fn tag_hex_u32(trailer: &str, key: &str) -> Option<u32> {
    let prefix = format!("{key}=");
    trailer
        .split_whitespace()
        .find_map(|tok| u32::from_str_radix(tok.strip_prefix(prefix.as_str())?, 16).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_published_test_vector() {
        // "123456789" -> 0xCBF43926
        assert_eq!(crc32(b"123456789"), 0xCBF43926);
    }

    #[test]
    fn round_trip_covers_every_byte_value() {
        let all: Vec<u8> = (0..=255u8).collect();
        let text = encode(&all, "all.bin");
        assert_eq!(decode(&text).expect("decode"), all);
    }

    #[test]
    fn escaping_is_present_for_special_bytes() {
        // Plaintext bytes whose +42 lands on NUL/LF/CR/'=' must be escaped.
        let data = [214u8, 224, 227, 19];
        let text = encode(&data, "esc.bin");
        assert!(text.contains(&b'='), "expected escape sequences");
        assert_eq!(decode(&text).expect("decode"), data);
    }

    #[test]
    fn tampered_body_fails_crc() {
        let data = b"hello streaming world, this is a longer payload for crc";
        let mut text = encode(data, "t.bin");
        // Flip the first body byte (right after the =ybegin line).
        let body_pos = text.iter().position(|&b| b == b'\n').unwrap() + 1;
        text[body_pos] = text[body_pos].wrapping_add(1);
        assert!(matches!(decode(&text), Err(YencError::CrcMismatch { .. })));
    }

    #[test]
    fn missing_trailer_is_truncated() {
        let text = encode(b"some data here", "x.bin");
        let pos = text
            .windows(5)
            .rposition(|w| w == b"=yend")
            .expect("trailer present");
        assert_eq!(decode(&text[..pos]), Err(YencError::Truncated));
    }
}
