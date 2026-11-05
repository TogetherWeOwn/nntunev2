//! Service-level pool provider registry (ADR 0014 foundation).
//!
//! Pool providers are operator-owned NNTP accounts shared by the whole
//! service, not per-user vault rows. This module holds the settings and
//! write contracts, redacted views/audits, and the password secret envelope.
//!
//! Secret envelope: the shared API v2 field envelope
//! (one implementation, no second crypto path),
//! bound to AAD table `pool_provider_secrets`, owner scope `service`
//! and row key `secret_ref` (a UUID). Because the table and scope differ
//! from the legacy `providers` rows, a pool envelope never authenticates
//! as a legacy provider envelope and vice versa. Decrypt is pinned to the
//! stored KEK version (no fallback loop).
//!
//! No type here derives `Debug`: settings carry the NNTP username and
//! the secret carries the password (threat-model §3).

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::field_envelope::{self, Context};
use crate::vault::{host_shape_ok, nntp_port_allowed, VaultError, MAX_CONNECTIONS_CEILING};

/// Stored pool-secret bytes; no plaintext DEK and deliberately no Debug.
pub use crate::field_envelope::FieldEnvelope as PoolSecretEnvelope;

/// Schema-pinned v2 envelope and JSON payload format.
pub const CREDS_ENVELOPE_VERSION: i16 = 2;
pub const CREDS_PAYLOAD_FORMAT_JSON_V1: i16 = 1;

fn secret_context(secret_ref: &str) -> Context<'_> {
    Context {
        table: POOL_SECRET_AAD_TABLE,
        field: "encrypted_creds",
        scope: POOL_SECRET_OWNER_SCOPE,
        row: secret_ref,
        version: CREDS_ENVELOPE_VERSION as u16,
        format: CREDS_PAYLOAD_FORMAT_JSON_V1 as u16,
    }
}

/// AAD table pin for pool secrets.
pub const POOL_SECRET_AAD_TABLE: &str = "pool_provider_secrets";
/// AAD owner scope for every pool secret (service-owned, not per user).
pub const POOL_SECRET_OWNER_SCOPE: &str = "service";

/// Longest display label (`pool_providers.label` CHECK).
pub const MAX_LABEL_CHARS: usize = 64;
/// Longest backbone tag (`pool_providers.backbone` CHECK).
pub const MAX_BACKBONE_CHARS: usize = 64;
/// Largest priority tier (`pool_providers.tier` CHECK).
pub const MAX_TIER: u32 = 100;
/// Longest username (matches the vault bound).
pub const MAX_USERNAME_BYTES: usize = 255;
/// Longest password (matches the vault bound).
pub const MAX_PASSWORD_BYTES: usize = 1024;

/// Non-secret-envelope settings for one pool account. Carries the NNTP
/// username, so deliberately no `Debug`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolProviderSettings {
    pub id: String,
    pub label: String,
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub username: String,
    pub can_read: bool,
    pub can_post: bool,
    pub max_connections: u32,
    pub stream_reserve: u32,
    pub ingest_limit: u32,
    pub backbone: String,
    pub tier: u32,
    pub enabled: bool,
}

fn has_control(s: &str) -> bool {
    s.chars().any(|c| c.is_control())
}

pub fn lower_uuid_ok(raw: &str) -> bool {
    crate::meter_sink::is_uuid_shaped(raw) && !raw.bytes().any(|b| b.is_ascii_uppercase())
}

/// Fresh secret identity; randomness failure must not reuse an old identity.
pub(crate) fn mint_secret_ref() -> Result<String, VaultError> {
    let mut b = crate::random::secure_random_bytes::<16>()?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12],
        b[13], b[14], b[15]
    ))
}

impl PoolProviderSettings {
    /// Validate every field. Errors are static, non-echoing strings.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !lower_uuid_ok(&self.id) {
            return Err("id must be a lowercase 8-4-4-4-12 hex uuid");
        }
        if self.label.trim().is_empty()
            || self.label.chars().count() > MAX_LABEL_CHARS
            || has_control(&self.label)
        {
            return Err("label must be 1..=64 printable chars");
        }
        if !host_shape_ok(&self.host) {
            return Err("host must be a bare hostname or IP literal");
        }
        if !nntp_port_allowed(self.port) {
            return Err("port is not on the NNTP allowlist");
        }
        if !self.tls {
            return Err("tls is required");
        }
        if self.username.is_empty()
            || self.username.len() > MAX_USERNAME_BYTES
            || has_control(&self.username)
        {
            return Err("username must be 1..=255 bytes without control chars");
        }
        if self.max_connections == 0 || self.max_connections > MAX_CONNECTIONS_CEILING {
            return Err("max_connections must be 1..=50");
        }
        if self.stream_reserve > self.max_connections {
            return Err("stream_reserve must not exceed max_connections");
        }
        if self.can_read {
            if self.stream_reserve == 0 {
                return Err("stream_reserve must be at least 1 for read accounts");
            }
            if self.ingest_limit > self.max_connections - self.stream_reserve {
                return Err("ingest_limit must not exceed max_connections - stream_reserve");
            }
        } else if self.ingest_limit != 0 {
            return Err("ingest_limit must be 0 for accounts that cannot read");
        }
        let b = &self.backbone;
        if b.is_empty()
            || b.chars().count() > MAX_BACKBONE_CHARS
            || !b
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
        {
            return Err("backbone must be 1..=64 of [A-Za-z0-9._-]");
        }
        if self.tier > MAX_TIER {
            return Err("tier must be 0..=100");
        }
        Ok(())
    }
}

/// The pool account password, JSON-serialized into the envelope.
/// Cleared on drop; no `Debug`.
#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub struct PoolProviderSecret {
    pub password: String,
}

impl PoolProviderSecret {
    pub fn validate(&self) -> Result<(), VaultError> {
        // CR, LF and NUL would break NNTP command framing (the
        // transport refuses them), so they are rejected at the source.
        if self.password.is_empty()
            || self.password.len() > MAX_PASSWORD_BYTES
            || self
                .password
                .bytes()
                .any(|b| matches!(b, b'\r' | b'\n' | 0))
        {
            return Err(VaultError::Invalid);
        }
        Ok(())
    }
}

fn check_ref(secret_ref: &str) -> Result<(), VaultError> {
    if lower_uuid_ok(secret_ref) {
        Ok(())
    } else {
        Err(VaultError::Invalid)
    }
}

/// Encrypt a pool secret bound to `secret_ref`. Fail-closed on invalid
/// input or unavailable randomness.
pub fn encrypt_pool_secret(
    kek: &[u8; 32],
    kek_version: u32,
    secret_ref: &str,
    secret: &PoolProviderSecret,
) -> Result<PoolSecretEnvelope, VaultError> {
    check_ref(secret_ref)?;
    secret.validate()?;
    let mut plaintext = serde_json::to_vec(secret).map_err(|_| VaultError::Crypto)?;
    let out = field_envelope::seal(kek, kek_version, &secret_context(secret_ref), &plaintext);
    plaintext.zeroize();
    out
}

/// Decrypt a pool secret. `kek` must be the KEK for the row's stored
/// `kek_version` (version-pinned; no fallback).
pub fn decrypt_pool_secret(
    kek: &[u8; 32],
    kek_version: u32,
    secret_ref: &str,
    envelope: &PoolSecretEnvelope,
) -> Result<PoolProviderSecret, VaultError> {
    check_ref(secret_ref)?;
    let mut plaintext =
        field_envelope::open(kek, kek_version, &secret_context(secret_ref), envelope)?;
    let parsed = serde_json::from_slice::<PoolProviderSecret>(&plaintext)
        .map_err(|_| VaultError::CorruptStoredData);
    plaintext.zeroize();
    let secret = parsed?;
    secret
        .validate()
        .map_err(|_| VaultError::CorruptStoredData)?;
    Ok(secret)
}

/// Rewrap the DEK under a new KEK/version. Ciphertext and payload nonce
/// are unchanged; only the wrap nonce and wrapped DEK are new. The row
/// binding (`secret_ref`) is unchanged.
pub fn rewrap_pool_secret(
    old_kek: &[u8; 32],
    old_version: u32,
    new_kek: &[u8; 32],
    new_version: u32,
    secret_ref: &str,
    envelope: &PoolSecretEnvelope,
) -> Result<PoolSecretEnvelope, VaultError> {
    check_ref(secret_ref)?;
    field_envelope::rewrap(
        old_kek,
        old_version,
        new_kek,
        new_version,
        &secret_context(secret_ref),
        envelope,
    )
}

/// Stored registry metadata. The password envelope is deliberately separate;
/// admin reads cannot accidentally return it. No credential-bearing Debug.
#[derive(Clone)]
pub struct PoolProviderRow {
    pub settings: PoolProviderSettings,
    pub secret_ref: String,
    pub revision: u64,
}

impl PoolProviderRow {
    /// Explicit response allowlist: neither username nor secret identity leaks
    /// through an admin read, an audit record, or a successful write response.
    pub fn view(&self) -> serde_json::Value {
        let s = &self.settings;
        serde_json::json!({
            "id": s.id, "label": s.label, "host": s.host,
            "port": s.port, "tls": s.tls, "can_read": s.can_read,
            "can_post": s.can_post, "max_connections": s.max_connections,
            "stream_reserve": s.stream_reserve, "ingest_limit": s.ingest_limit,
            "backbone": s.backbone, "tier": s.tier, "enabled": s.enabled,
            "revision": self.revision,
        })
    }
}

/// Fresh envelope for a create or an explicit credential rotation. A rotation
/// always mints a new identity; settings-only writes retain the old identity
/// and therefore cannot clear an authentication quarantine.
pub struct PoolSecretWrite {
    pub secret_ref: String,
    pub kek_version: u32,
    pub envelope: PoolSecretEnvelope,
}

/// Compare-and-swap registry write. Revision zero means create, never upsert.
/// The store revalidates all inputs before touching persistent state.
pub struct PoolProviderWrite {
    pub settings: PoolProviderSettings,
    pub expected_revision: u64,
    pub secret: Option<PoolSecretWrite>,
}

impl PoolProviderWrite {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.settings.validate()?;
        if self.expected_revision >= i64::MAX as u64 {
            return Err("invalid revision");
        }
        match &self.secret {
            None if self.expected_revision == 0 => Err("create requires credentials"),
            Some(secret) => {
                if !lower_uuid_ok(&secret.secret_ref)
                    || secret.kek_version == 0
                    || secret.kek_version > i32::MAX as u32
                    || secret.envelope.dek_wrapped.len() != 48
                    || secret.envelope.ciphertext.len() < 16
                {
                    return Err("invalid secret envelope");
                }
                Ok(())
            }
            None => Ok(()),
        }
    }
}

pub enum PoolProviderWriteOutcome {
    Applied(PoolProviderRow),
    ActorNotAdmin,
    Conflict,
}

/// Fixed audit vocabulary plus redacted before/after views. Every affected
/// category is recorded in the same transaction as the registry mutation.
pub fn audit_changes(
    previous: Option<&PoolProviderRow>,
    next: &PoolProviderRow,
    rotated: bool,
) -> Vec<(&'static str, String)> {
    let mut actions = vec![if previous.is_none() {
        "pool_provider.created"
    } else {
        "pool_provider.updated"
    }];
    if let Some(old) = previous {
        if old.settings.enabled && !next.settings.enabled {
            actions.push("pool_provider.disabled");
        }
        if old.settings.max_connections != next.settings.max_connections
            || old.settings.stream_reserve != next.settings.stream_reserve
            || old.settings.ingest_limit != next.settings.ingest_limit
        {
            actions.push("pool_provider.caps_changed");
        }
        if rotated {
            actions.push("pool_provider.credentials_rotated");
        }
    }
    let details = serde_json::json!({
        "provider_id": next.settings.id,
        "old": previous.map(PoolProviderRow::view),
        "new": next.view(),
    })
    .to_string();
    actions
        .into_iter()
        .map(|action| (action, details.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET_REF: &str = "11111111-1111-4111-8111-111111111111";

    #[test]
    fn pool_and_legacy_provider_envelopes_do_not_cross() {
        let k = [7; 32];
        let pw = br#"{"password":"pw"}"#;
        let pool = encrypt_pool_secret(
            &k,
            1,
            SECRET_REF,
            &PoolProviderSecret {
                password: "pw".into(),
            },
        )
        .unwrap();
        // Legacy contexts exist only in this fixture, not a credential writer.
        for owner in ["service", "user-1", ""] {
            let legacy_context = Context {
                table: "providers",
                field: "encrypted_creds",
                scope: owner,
                row: SECRET_REF,
                version: 2,
                format: 1,
            };
            assert!(field_envelope::open(&k, 1, &legacy_context, &pool).is_err());
            let legacy = field_envelope::seal(&k, 1, &legacy_context, pw).unwrap();
            assert!(decrypt_pool_secret(&k, 1, SECRET_REF, &legacy).is_err());
            assert_eq!(
                field_envelope::open(&k, 1, &legacy_context, &legacy).unwrap(),
                pw
            );
        }
        assert_eq!(
            decrypt_pool_secret(&k, 1, SECRET_REF, &pool)
                .unwrap()
                .password,
            "pw"
        );
    }

    #[test]
    fn secret_refs_are_fresh_lowercase_v4_uuids() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..32 {
            let id = mint_secret_ref().unwrap();
            assert!(lower_uuid_ok(&id));
            assert_eq!(id.as_bytes()[14], b'4');
            assert!(matches!(id.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
            assert!(seen.insert(id));
        }
    }

    #[test]
    fn lowercase_uuid_shape_is_unchanged() {
        for bad in [
            "",
            "not-a-uuid",
            "11111111111141118111111111111111",
            "11111111-1111-4111-8111-11111111111g",
            "ABCDEF12-1234-4234-8234-123456789ABC",
            "11111111-1111-4111-8111-111111111111-",
            "é1111111-1111-4111-8111-111111111111",
        ] {
            assert!(!lower_uuid_ok(bad));
        }
        // Shape, not a new version/variant constraint on existing identities.
        assert!(lower_uuid_ok("abcdef12-1234-0234-0234-123456789abc"));
    }
}
