//! Minimal yEnc encode/decode helpers for fixtures and tests.
//!
//! yEnc (http://www.yenc.org): each source byte `b` is emitted as
//! `(b + 42) % 256`, with `0x00`, `0x0A`, `0x0D`, `0x3D` escaped as
//! `=`, followed by `(c + 64) % 256`.
//!
//! All functions are byte-oriented: yEnc output is raw bytes 0-255, not
//! UTF-8 text, so `encode` returns `Vec<u8>`.

/// Encode raw bytes into a yEnc body (no framing), wrapped at `line_len`.
pub fn encode(data: &[u8], line_len: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut col = 0usize;
    let mut push = |byte: u8| {
        out.push(byte);
        col += 1;
        if col >= line_len {
            out.extend_from_slice(b"\r\n");
            col = 0;
        }
    };
    for &b in data {
        let c = b.wrapping_add(42);
        match c {
            0x00 | 0x0A | 0x0D | 0x3D => {
                push(b'=');
                push(c.wrapping_add(64));
            }
            _ => push(c),
        }
    }
    out
}

/// Decode a yEnc body (framing lines ignored) back to raw bytes.
///
/// Line breaks are removed *before* unescaping so an escape pair split
/// across the encoder's line wrap still decodes correctly.
pub fn decode(body: &[u8]) -> Vec<u8> {
    // Join non-framing lines first (strip CRLF), then unescape globally.
    let mut joined = Vec::new();
    for line in body.split(|&b| b == b'\n') {
        let line = if line.last() == Some(&b'\r') {
            &line[..line.len() - 1]
        } else {
            line
        };
        if line.starts_with(b"=y") {
            continue;
        }
        joined.extend_from_slice(line);
    }
    let mut out = Vec::with_capacity(joined.len());
    let mut it = joined.iter();
    while let Some(&b) = it.next() {
        if b == b'=' {
            if let Some(&n) = it.next() {
                out.push(n.wrapping_sub(64).wrapping_sub(42));
            }
        } else {
            out.push(b.wrapping_sub(42));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_with_escaped_bytes() {
        let data: Vec<u8> = (0u8..=255u8).collect();
        let enc = encode(&data, 128);
        assert_eq!(decode(&enc), data);
    }

    #[test]
    fn roundtrip_with_framing() {
        let data = b"hello nntune binary \x00\xff\x0a\x0d=";
        let mut framed = b"=ybegin line=128 size=22 name=test.bin\r\n".to_vec();
        framed.extend(encode(data, 128));
        framed.extend_from_slice(b"\r\n=yend\r\n");
        assert_eq!(decode(&framed), data);
    }
}
