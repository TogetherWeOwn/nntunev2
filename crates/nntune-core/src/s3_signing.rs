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

    /// Sign an already canonical S3 request. `signed_headers` must be its exact
    /// lowercase header list, and `date` its signed UTC timestamp.
    pub fn authorization(
        &self,
        date: &str,
        canonical_request: &str,
        signed_headers: &str,
    ) -> Option<Zeroizing<String>> {
        if date.len() != 16
            || !date.bytes().enumerate().all(|(index, byte)| match index {
                8 => byte == b'T',
                15 => byte == b'Z',
                _ => byte.is_ascii_digit(),
            })
        {
            return None;
        }
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
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            output.push(char::from(byte));
        } else {
            let _ = write!(output, "%{byte:02X}");
        }
    }
    output
}
