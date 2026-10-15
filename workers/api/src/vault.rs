//! BYOP credential vault: envelope encryption + validation + SSRF guard.
//!
//! Design (threat-model §3):
//! - Per-user DEK (random 256-bit), wrapped by a KEK from the Worker secret.
//!   KEK version column; staging and prod KEKs never shared; rotation via
//!   [`rewrap_dek`] without touching ciphertext.
//! - AEAD (AES-256-GCM) with per-row random 96-bit nonces. Associated data
//!   binds `user_id` (ciphertext swapped between rows fails to decrypt).
//! - Credentials are never logged and never returned after save: reads
//!   return [`redacted_summary`] (booleans only); `test connection`
//!   returns a boolean only, never provider banners.
//! - User-supplied provider hosts are hostile input: [`ProviderConfig::validate`]
//!   enforces shape + the SSRF guard (resolve ALL records, reject
//!   loopback/link-local/private/unique-local/reserved) before any dial.
//! - Timeouts and caps: [`DIAL_TIMEOUT_MS`], [`TOTAL_TIMEOUT_MS`],
//!   [`MAX_TEST_CONCURRENCY`], [`MAX_CONNECTIONS_CEILING`].

use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Dial timeout for `test connection` (ms).
pub const DIAL_TIMEOUT_MS: u64 = 5_000;
/// Total timeout for `test connection` (ms).
pub const TOTAL_TIMEOUT_MS: u64 = 15_000;
/// Per-user concurrent `test connection` cap.
pub const MAX_TEST_CONCURRENCY: u32 = 2;
/// Maximum vetted DNS addresses attempted by one test-connection request.
pub const MAX_DIAL_ADDRESSES: usize = 4;
/// `max_connections` ceiling: the vault row value is a cap, not a target.
pub const MAX_CONNECTIONS_CEILING: u32 = 50;
/// Indexer fetches must never follow redirects (SSRF).
pub const INDEXER_ALLOW_REDIRECTS: bool = false;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultError {
    Invalid,
    Crypto,
}

impl std::fmt::Display for VaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Generic: never echo hosts, secrets, or crypto details.
        write!(f, "vault unavailable")
    }
}

// --- Config shapes --------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub username: String,
    pub password: String,
    pub max_connections: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexerConfig {
    pub url: String,
    pub api_key: String,
}

/// DNS resolution as a trait so tests use a fake and the Worker uses real DNS.
pub trait DnsResolver {
    fn resolve(&self, host: &str) -> Vec<IpAddr>;
}

fn host_shape_ok(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    // Never a URL: no scheme, no path, no userinfo, no port suffix.
    if host.contains("://")
        || host.contains('/')
        || host.contains('@')
        || host.contains(':')
        || host.contains(' ')
    {
        return false;
    }
    // DNS name labels or an IP literal.
    if host.parse::<IpAddr>().is_ok() {
        return true;
    }
    !host.starts_with('.')
        && !host.ends_with('.')
        && !host.contains("..")
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
}

fn v4_is_forbidden(o: &[u8; 4]) -> bool {
    let v4 = Ipv4Addr::from(*o);
    v4.is_loopback()         // 127/8
        || v4.is_multicast() // 224.0.0.0/4
        || v4.is_link_local() // 169.254/16
        || v4.is_private() // 10/8, 172.16/12, 192.168/16
        || v4.is_broadcast()
        || v4.is_documentation() // 192.0.2/24, 198.51.100/24, 203.0.113/24
        || o[0] == 0 // 0.0.0.0/8 ("this network")
        || (o[0] == 100 && (o[1] & 0xC0) == 64) // 100.64/10 carrier-grade NAT
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24 (IETF protocol assignments)
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19)) // 198.18.0.0/15 benchmarking
        || o[0] >= 240 // 240.0.0.0/4 reserved + 255.255.255.255 broadcast
}

fn ip_is_forbidden(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_is_forbidden(&v4.octets()),
        IpAddr::V6(v6) => {
            let s = v6.segments();
            // Only native global unicast (2000::/3). This excludes NAT64,
            // mapped/translated/compatible IPv4, site-local and discard.
            // Also reject transition and special-use space inside that range
            // rather than trusting a translator's embedded IPv4 destination.
            (s[0] & 0xE000) != 0x2000
                || s[0] == 0x2002 // 6to4 (2002::/16)
                || (s[0] == 0x2001 && s[1] < 0x0200) // IETF special-use, incl. Teredo/ORCHID
                || (s[0] == 0x2001 && s[1] == 0x0DB8) // documentation
                || (s[0] == 0x3FFF && (s[1] & 0xF000) == 0) // documentation (3fff::/20)
        }
    }
}

/// Vetted destination for one immediate outbound connection. Do not store
/// this across requests: validate again at dial/fetch time, connect only to
/// an address from this set, and retain `host` for TLS SNI/certificate checks
/// and the HTTP Host header. Never let the transport re-resolve the hostname.
#[derive(Debug)]
pub struct ValidatedTarget {
    host: String,
    addresses: Vec<IpAddr>,
}

impl ValidatedTarget {
    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn addresses(&self) -> &[IpAddr] {
        &self.addresses
    }
}

fn resolve_public_target(
    host: &str,
    dns: &impl DnsResolver,
) -> Result<ValidatedTarget, VaultError> {
    let mut addresses = match host.parse::<IpAddr>() {
        Ok(ip) => vec![ip],
        Err(_) => dns.resolve(host),
    };
    if addresses.is_empty() || addresses.iter().any(ip_is_forbidden) {
        return Err(VaultError::Invalid);
    }
    // Validate every answer before retaining a bounded prefix for dialing;
    // a later forbidden DNS answer must still reject the whole target.
    addresses.truncate(MAX_DIAL_ADDRESSES);
    Ok(ValidatedTarget {
        host: host.to_string(),
        addresses,
    })
}

impl ProviderConfig {
    /// Validate shape + SSRF guard across ALL resolved addresses, retaining
    /// at most [`MAX_DIAL_ADDRESSES`] for a pinned dial. Call at save AND
    /// immediately before dial. Every failure
    /// maps to generic [`VaultError::Invalid`] (no target/existence oracle).
    pub fn validate(&self, dns: &impl DnsResolver) -> Result<ValidatedTarget, VaultError> {
        if !host_shape_ok(&self.host) {
            return Err(VaultError::Invalid);
        }
        if self.port == 0 {
            return Err(VaultError::Invalid);
        }
        if self.username.is_empty() || self.username.len() > 255 {
            return Err(VaultError::Invalid);
        }
        if self.password.is_empty() || self.password.len() > 1024 {
            return Err(VaultError::Invalid);
        }
        if self.max_connections == 0 || self.max_connections > MAX_CONNECTIONS_CEILING {
            return Err(VaultError::Invalid);
        }
        resolve_public_target(&self.host, dns)
    }
}

impl IndexerConfig {
    /// Validate the indexer URL + key, including the same SSRF guard as
    /// providers: the URL host is user-supplied and later fetched, so
    /// non-public targets (metadata endpoints, loopback, RFC 1918) are
    /// rejected at save time AND immediately before fetch. Use the returned
    /// addresses for a pinned fetch; redirects stay forbidden
    /// ([`INDEXER_ALLOW_REDIRECTS`]).
    pub fn validate(&self, dns: &impl DnsResolver) -> Result<ValidatedTarget, VaultError> {
        let parsed = url::Url::parse(&self.url).map_err(|_| VaultError::Invalid)?;
        if parsed.scheme() != "https" && parsed.scheme() != "http" {
            return Err(VaultError::Invalid);
        }
        let host = parsed
            .host_str()
            .filter(|h| !h.is_empty())
            .ok_or(VaultError::Invalid)?;
        // URL hosts may carry brackets (IPv6) or a port — strip both, then
        // run the same shape + resolve-ALL + reject-non-public guard.
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        if !host_shape_ok(bare) {
            return Err(VaultError::Invalid);
        }
        if self.api_key.is_empty() || self.api_key.len() > 1024 {
            return Err(VaultError::Invalid);
        }
        resolve_public_target(bare, dns)
    }
}

// --- Envelope encryption -----------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultBlob {
    pub kek_version: u32,
    pub dek_wrapped_nonce_b64url: String,
    pub dek_wrapped_b64url: String,
    pub payload_nonce_b64url: String,
    pub payload_b64url: String,
}

#[derive(Zeroize, ZeroizeOnDrop)]
struct Dek([u8; 32]);

fn random_nonce() -> [u8; 12] {
    let mut n = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut n);
    n
}

fn b64url(bytes: &[u8]) -> String {
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

fn unb64url(s: &str) -> Result<Vec<u8>, VaultError> {
    fn val(c: u8) -> Result<u32, VaultError> {
        match c {
            b'A'..=b'Z' => Ok((c - b'A') as u32),
            b'a'..=b'z' => Ok((c - b'a' + 26) as u32),
            b'0'..=b'9' => Ok((c - b'0' + 52) as u32),
            b'-' => Ok(62),
            b'_' => Ok(63),
            _ => Err(VaultError::Invalid),
        }
    }
    if s.len() % 4 == 1 {
        return Err(VaultError::Invalid);
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        let mut present = 0usize;
        for &c in chunk {
            n = (n << 6) | val(c)?;
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
    Ok(out)
}

fn dek_aad(user_id: &str, kek_version: u32) -> String {
    format!("v1|dek|{}|{}", user_id, kek_version)
}

fn payload_aad(user_id: &str) -> String {
    format!("v1|vault|{}", user_id)
}

/// Encrypt `plaintext` (serialized provider+indexer secrets) for `user_id`.
pub fn encrypt_vault(
    kek: &[u8; 32],
    kek_version: u32,
    user_id: &str,
    plaintext: &[u8],
) -> Result<VaultBlob, VaultError> {
    let mut dek_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut dek_bytes);
    let dek = Dek(dek_bytes);
    dek_bytes.zeroize();

    // Wrap the DEK with the KEK (AAD binds user + KEK version).
    let kek_cipher = Aes256Gcm::new_from_slice(kek).map_err(|_| VaultError::Crypto)?;
    let wrap_nonce = random_nonce();
    let wrapped = kek_cipher
        .encrypt(
            &Nonce::from(wrap_nonce),
            Payload {
                msg: &dek.0,
                aad: dek_aad(user_id, kek_version).as_bytes(),
            },
        )
        .map_err(|_| VaultError::Crypto)?;

    // Encrypt the payload with the DEK (AAD binds user).
    let dek_cipher = Aes256Gcm::new_from_slice(&dek.0).map_err(|_| VaultError::Crypto)?;
    let payload_nonce = random_nonce();
    let payload = dek_cipher
        .encrypt(
            &Nonce::from(payload_nonce),
            Payload {
                msg: plaintext,
                aad: payload_aad(user_id).as_bytes(),
            },
        )
        .map_err(|_| VaultError::Crypto)?;

    Ok(VaultBlob {
        kek_version,
        dek_wrapped_nonce_b64url: b64url(&wrap_nonce),
        dek_wrapped_b64url: b64url(&wrapped),
        payload_nonce_b64url: b64url(&payload_nonce),
        payload_b64url: b64url(&payload),
    })
}

/// Decrypt a vault row. The caller must have already enforced the owner
/// check (authorization precedes lookup: non-owners get the same response
/// for "no vault" as "not yours" — no existence oracle).
pub fn decrypt_vault(
    kek: &[u8; 32],
    user_id: &str,
    blob: &VaultBlob,
) -> Result<Vec<u8>, VaultError> {
    let kek_cipher = Aes256Gcm::new_from_slice(kek).map_err(|_| VaultError::Crypto)?;
    let wrap_nonce: [u8; 12] = unb64url(&blob.dek_wrapped_nonce_b64url)?
        .try_into()
        .map_err(|_| VaultError::Invalid)?;
    let wrapped = unb64url(&blob.dek_wrapped_b64url)?;
    let mut dek_bytes = kek_cipher
        .decrypt(
            &Nonce::from(wrap_nonce),
            Payload {
                msg: &wrapped,
                aad: dek_aad(user_id, blob.kek_version).as_bytes(),
            },
        )
        .map_err(|_| VaultError::Crypto)?;
    if dek_bytes.len() != 32 {
        dek_bytes.zeroize();
        return Err(VaultError::Crypto);
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&dek_bytes);
    // Clear the AES-GCM output buffer: the DEK now lives only in `dek`.
    dek_bytes.zeroize();
    let dek = Dek(arr);

    let dek_cipher = Aes256Gcm::new_from_slice(&dek.0).map_err(|_| VaultError::Crypto)?;
    let payload_nonce: [u8; 12] = unb64url(&blob.payload_nonce_b64url)?
        .try_into()
        .map_err(|_| VaultError::Invalid)?;
    let payload = unb64url(&blob.payload_b64url)?;
    dek_cipher
        .decrypt(
            &Nonce::from(payload_nonce),
            Payload {
                msg: &payload,
                aad: payload_aad(user_id).as_bytes(),
            },
        )
        .map_err(|_| VaultError::Crypto)
}

/// Rotate the KEK: unwrap the DEK with the old KEK, re-wrap with the new
/// KEK at `new_version`. Payload ciphertext is untouched.
pub fn rewrap_dek(
    old_kek: &[u8; 32],
    new_kek: &[u8; 32],
    new_version: u32,
    user_id: &str,
    blob: &VaultBlob,
) -> Result<VaultBlob, VaultError> {
    // Unwrap with the old KEK (old AAD: old version).
    let old_cipher = Aes256Gcm::new_from_slice(old_kek).map_err(|_| VaultError::Crypto)?;
    let wrap_nonce: [u8; 12] = unb64url(&blob.dek_wrapped_nonce_b64url)?
        .try_into()
        .map_err(|_| VaultError::Invalid)?;
    let wrapped = unb64url(&blob.dek_wrapped_b64url)?;
    let mut dek_bytes = old_cipher
        .decrypt(
            &Nonce::from(wrap_nonce),
            Payload {
                msg: &wrapped,
                aad: dek_aad(user_id, blob.kek_version).as_bytes(),
            },
        )
        .map_err(|_| VaultError::Crypto)?;
    if dek_bytes.len() != 32 {
        dek_bytes.zeroize();
        return Err(VaultError::Crypto);
    }

    // Re-wrap with the new KEK (new AAD: new version).
    let new_cipher = Aes256Gcm::new_from_slice(new_kek).map_err(|_| VaultError::Crypto)?;
    let new_nonce = random_nonce();
    let wrap_result = new_cipher.encrypt(
        &Nonce::from(new_nonce),
        Payload {
            msg: &dek_bytes,
            aad: dek_aad(user_id, new_version).as_bytes(),
        },
    );
    // Clear the unwrapped DEK regardless of the re-wrap outcome.
    dek_bytes.zeroize();
    let new_wrapped = wrap_result.map_err(|_| VaultError::Crypto)?;

    Ok(VaultBlob {
        kek_version: new_version,
        dek_wrapped_nonce_b64url: b64url(&new_nonce),
        dek_wrapped_b64url: b64url(&new_wrapped),
        payload_nonce_b64url: blob.payload_nonce_b64url.clone(),
        payload_b64url: blob.payload_b64url.clone(),
    })
}

// --- Redaction + audit (never secrets) --------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultSummary {
    pub provider_configured: bool,
    pub indexer_configured: bool,
    pub updated_at: u64,
}

/// The ONLY vault read shape: booleans, no host/user/pass/URL/key material.
pub fn redacted_summary(
    provider_configured: bool,
    indexer_configured: bool,
    updated_at: u64,
) -> VaultSummary {
    VaultSummary {
        provider_configured,
        indexer_configured,
        updated_at,
    }
}

/// `test connection` returns a boolean only — never provider banners.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestConnectionResult {
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub user_id: String,
    pub action: String,
    pub ok: bool,
    pub source_ip: String,
    pub at: u64,
}

/// Audit log event. By construction there is no field for secret material:
/// callers pass ids and results only. `now` is Unix epoch seconds — the
/// audit row's ordering depends on it, so it is a required parameter.
pub fn audit_event(user_id: &str, action: &str, ok: bool, source_ip: &str, now: u64) -> AuditEvent {
    AuditEvent {
        user_id: user_id.to_string(),
        action: action.to_string(),
        ok,
        source_ip: source_ip.to_string(),
        at: now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EmptyDns;
    impl DnsResolver for EmptyDns {
        fn resolve(&self, _host: &str) -> Vec<IpAddr> {
            vec![]
        }
    }

    struct ListDns(Vec<IpAddr>);
    impl DnsResolver for ListDns {
        fn resolve(&self, _host: &str) -> Vec<IpAddr> {
            self.0.clone()
        }
    }

    #[test]
    fn unresolvable_host_rejected() {
        let c = ProviderConfig {
            host: "nope.example.com".into(),
            port: 119,
            tls: false,
            username: "u".into(),
            password: "p".into(),
            max_connections: 2,
        };
        assert!(c.validate(&EmptyDns).is_err());
    }

    #[test]
    fn validated_target_retains_only_a_bounded_address_prefix_after_checking_all() {
        let config = ProviderConfig {
            host: "news.example.com".into(),
            port: 119,
            tls: false,
            username: "u".into(),
            password: "p".into(),
            max_connections: 2,
        };
        let public = ListDns(vec![
            "8.8.8.8".parse().unwrap(),
            "1.1.1.1".parse().unwrap(),
            "9.9.9.9".parse().unwrap(),
            "4.2.2.1".parse().unwrap(),
            "208.67.222.222".parse().unwrap(),
        ]);
        let target = config.validate(&public).unwrap();
        assert_eq!(target.addresses().len(), MAX_DIAL_ADDRESSES);

        let forbidden_after_prefix = ListDns(vec![
            "8.8.8.8".parse().unwrap(),
            "1.1.1.1".parse().unwrap(),
            "9.9.9.9".parse().unwrap(),
            "4.2.2.1".parse().unwrap(),
            "10.0.0.1".parse().unwrap(),
        ]);
        assert!(config.validate(&forbidden_after_prefix).is_err());
    }

    #[test]
    fn ipv6_remote_blocked() {
        for host in ["::1", "fe80::1", "fc00::1", "ff02::1"] {
            let c = ProviderConfig {
                host: host.into(),
                port: 119,
                tls: false,
                username: "u".into(),
                password: "p".into(),
                max_connections: 2,
            };
            assert!(c.validate(&EmptyDns).is_err(), "blocked: {host}");
        }
    }
}
