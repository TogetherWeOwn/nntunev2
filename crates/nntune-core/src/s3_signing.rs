//! SigV4 for the configured S3 endpoint. Callers build and send the same canonical request.

use std::fmt::Write as _;

use hkdf::hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

pub struct S3SigningKey {
    region: String,
    access_key: String,
    secret_key: Zeroizing<String>,
}

impl S3SigningKey {
    pub fn new(region: String, access_key: String, secret_key: String) -> Option<Self> {
        let secret_key = Zeroizing::new(secret_key);
        ((1..=64).contains(&region.len())
            && region
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && (1..=128).contains(&access_key.len())
            && access_key.bytes().all(|b| b.is_ascii_alphanumeric())
            && (1..=256).contains(&secret_key.len())
            && secret_key.bytes().all(|b| b.is_ascii_graphic()))
        .then_some(Self {
            region,
            access_key,
            secret_key,
        })
    }

    /// Sign a canonical S3 request using header authentication and a SHA-256
    /// payload hash. The caller supplies the current UTC date, computes the
    /// payload hash from the body and enforces its own request deadline.
    pub fn authorization(
        &self,
        date: &str,
        canonical_request: &str,
        signed_headers: &str,
    ) -> Option<Zeroizing<String>> {
        validate_timestamp(date)?;
        validate_request(canonical_request, signed_headers, date)?;
        let day = &date[..8];
        let scope = format!("{day}/{}/s3/aws4_request", self.region);
        let digest = hex(&Sha256::digest(canonical_request.as_bytes()));
        let message = format!("AWS4-HMAC-SHA256\n{date}\n{scope}\n{digest}");
        let secret = Zeroizing::new(format!("AWS4{}", self.secret_key.as_str()));
        let key = mac(secret.as_bytes(), day.as_bytes())?;
        let key = mac(key.as_ref(), self.region.as_bytes())?;
        let key = mac(key.as_ref(), b"s3")?;
        let key = mac(key.as_ref(), b"aws4_request")?;
        let signature = Zeroizing::new(hex(mac(key.as_ref(), message.as_bytes())?.as_ref()));
        Some(Zeroizing::new(format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={}",
            self.access_key,
            signature.as_str(),
        )))
    }
}

fn validate_timestamp(date: &str) -> Option<()> {
    if date.len() != 16
        || !date.bytes().enumerate().all(|(index, byte)| match index {
            8 => byte == b'T',
            15 => byte == b'Z',
            _ => byte.is_ascii_digit(),
        })
    {
        return None;
    }
    let year = date[..4].parse::<u16>().ok()?;
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = match &date[4..6] {
        "01" | "03" | "05" | "07" | "08" | "10" | "12" => "31",
        "04" | "06" | "09" | "11" => "30",
        "02" if leap => "29",
        "02" => "28",
        _ => return None,
    };
    (year != 0
        && ("01"..=days).contains(&&date[6..8])
        && &date[9..11] <= "23"
        && &date[11..13] <= "59"
        && &date[13..15] <= "59")
        .then_some(())
}

fn validate_request(request: &str, signed_headers: &str, date: &str) -> Option<()> {
    let (request, payload) = request.rsplit_once('\n')?;
    let mut lines = request.split('\n');
    let method = lines.next()?;
    let path = lines.next()?;
    let query = lines.next()?;
    if method.is_empty()
        || !method.bytes().all(|b| b.is_ascii_uppercase())
        || !path.starts_with('/')
        || path.split('/').any(|part| matches!(part, "." | ".."))
        || !canonical_encoding(path, true)
        || !valid_query(query)
        || payload.len() != 64
        || !payload
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    let (mut host, mut content_hash, mut timestamp) = (None, None, None);
    let mut previous = "";
    for name in signed_headers.split(';') {
        let (header, value) = lines.next()?.split_once(':')?;
        if header != name
            || name <= previous
            || name == "authorization"
            || !name
                .bytes()
                .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'))
            || !value.bytes().all(|b| b == b' ' || b.is_ascii_graphic())
            || value != value.trim_ascii()
            || value.contains("  ")
        {
            return None;
        }
        previous = name;
        match name {
            "host" => host = Some(value),
            "x-amz-content-sha256" => content_hash = Some(value),
            "x-amz-date" => timestamp = Some(value),
            _ => {}
        }
    }
    (host.is_some_and(|value| !value.is_empty() && !value.contains(' '))
        && content_hash == Some(payload)
        && timestamp == Some(date)
        && lines.next() == Some("")
        && lines.next() == Some(signed_headers)
        && lines.next().is_none())
    .then_some(())
}

fn valid_query(query: &str) -> bool {
    let mut previous = ("", "");
    query.is_empty() || query.split('&').all(|pair| {
        let Some((name, value)) = pair.split_once('=') else { return false };
        let ordered = previous <= (name, value);
        previous = (name, value);
        ordered && !name.is_empty() && canonical_encoding(name, false) && canonical_encoding(value, false)
            // Query authentication fields, including expiry, belong to presigning.
            && !name.get(..6).is_some_and(|prefix| prefix.eq_ignore_ascii_case("x-amz-"))
    })
}

fn unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-._~".contains(&byte)
}

fn canonical_encoding(value: &str, path: bool) -> bool {
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        if unreserved(byte) || (path && byte == b'/') {
            continue;
        }
        if byte != b'%' {
            return false;
        }
        let (Some(high), Some(low)) = (bytes.next(), bytes.next()) else {
            return false;
        };
        if [high, low].iter().any(u8::is_ascii_lowercase) {
            return false;
        }
        let (Some(high), Some(low)) = (char::from(high).to_digit(16), char::from(low).to_digit(16))
        else {
            return false;
        };
        let decoded = (high * 16 + low) as u8;
        if unreserved(decoded) || (path && decoded == b'/') {
            return false;
        }
    }
    true
}

fn mac(key: &[u8], message: &[u8]) -> Option<Zeroizing<[u8; 32]>> {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).ok()?;
    mac.update(message);
    Some(Zeroizing::new(mac.finalize().into_bytes().into()))
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

pub fn encode_query_value(value: &str) -> String {
    let mut output = String::new();
    for byte in value.bytes() {
        if unreserved(byte) {
            output.push(char::from(byte));
        } else {
            let _ = write!(output, "%{byte:02X}");
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_s3_signing_credentials() {
        let key = |region: &str, access: &str, secret: &str| {
            S3SigningKey::new(region.into(), access.into(), secret.into())
        };
        assert!(key("fixture-region", "FIXTUREACCESS", "fixture-secret").is_some());
        for region in ["", "Uppercase", "fixture/region", &"r".repeat(65)] {
            assert!(key(region, "FIXTUREACCESS", "fixture-secret").is_none());
        }
        for access in ["", "fixture/access", &"a".repeat(129)] {
            assert!(key("fixture-region", access, "fixture-secret").is_none());
        }
        for secret in ["", "fixture\nsecret", &"s".repeat(257)] {
            assert!(key("fixture-region", "FIXTUREACCESS", secret).is_none());
        }
    }

    #[test]
    fn requires_a_utc_signing_timestamp() {
        let key = S3SigningKey::new(
            "fixture-region".into(),
            "FIXTUREACCESS".into(),
            "fixture-secret".into(),
        )
        .unwrap();
        let authorize = |date| {
            let hash = hex(&Sha256::digest([]));
            let names = "host;x-amz-content-sha256;x-amz-date";
            let request = format!("HEAD\n/bucket/key\n\nhost:fixtures.invalid\nx-amz-content-sha256:{hash}\nx-amz-date:{date}\n\n{names}\n{hash}");
            key.authorization(date, &request, names)
        };
        assert!(authorize("20261124T074000Z").is_some());
        for invalid in [
            "",
            "20261124074000Z",
            "20261124t074000Z",
            "20261124T074000z",
            "202611xxT074000Z",
        ] {
            assert!(authorize(invalid).is_none());
        }
    }
}
