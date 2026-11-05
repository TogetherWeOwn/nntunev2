//! API-only service-account resolution. The dedicated HMAC authenticates the
//! stream Worker's service-binding requests, not users or admin sessions. The
//! KEK never leaves the API. Resolved credentials are ephemeral dial material:
//! never log, persist, queue, cache, or return them on a user-facing route.

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::pool_providers::{lower_uuid_ok, PoolProviderRow, PoolSecretEnvelope};

pub use nntune_core::pool_channel::{
    authorization_header, verify, AuthError, DIAL_MATERIAL_TTL_SECS, MAX_CLOCK_SKEW_SECS,
    MAX_REQUEST_BYTES, REGISTRY_PATH, RESOLVE_PATH, SECRET_VAR,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkClass {
    Stream,
    Ingest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryRequest {}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveRequest {
    pub provider_id: String,
    pub revision: u64,
    pub secret_ref: String,
    pub class: WorkClass,
}

impl ResolveRequest {
    pub fn valid(&self) -> bool {
        lower_uuid_ok(&self.provider_id)
            && lower_uuid_ok(&self.secret_ref)
            && self.revision > 0
            && self.revision <= i64::MAX as u64
    }

    /// A caller's snapshot is only a hint, never authority. Disabled,
    /// rotated, stale and missing accounts all have the same unavailable wire.
    pub fn matches(&self, row: &PoolProviderRow) -> bool {
        self.valid()
            && row.settings.validate().is_ok()
            && self.provider_id == row.settings.id
            && self.revision == row.revision
            && self.secret_ref == row.secret_ref
            && row.settings.enabled
            && row.settings.can_read
            && (self.class != WorkClass::Ingest || row.settings.ingest_limit > 0)
    }
}

/// Internal scheduling projection. No endpoint, username, password or envelope.
/// Includes disabled rows so a DO can reconcile kill switches and rotations.
#[derive(Serialize)]
pub struct AccountSnapshot<'a> {
    pub id: &'a str,
    pub revision: u64,
    pub secret_ref: &'a str,
    pub can_read: bool,
    pub max_connections: u32,
    pub stream_reserve: u32,
    pub ingest_limit: u32,
    pub tier: u32,
    pub enabled: bool,
}

impl<'a> From<&'a PoolProviderRow> for AccountSnapshot<'a> {
    fn from(row: &'a PoolProviderRow) -> Self {
        let s = &row.settings;
        Self {
            id: &s.id,
            revision: row.revision,
            secret_ref: &row.secret_ref,
            can_read: s.can_read,
            max_connections: s.max_connections,
            stream_reserve: s.stream_reserve,
            ingest_limit: s.ingest_limit,
            tier: s.tier,
            enabled: s.enabled,
        }
    }
}

/// Joined in one database statement: never pair a new endpoint/identity with
/// an envelope loaded from an older registry snapshot. No Debug.
pub struct StoredAccount {
    pub row: PoolProviderRow,
    pub kek_version: u32,
    pub envelope: PoolSecretEnvelope,
}

/// One immediate TLS dial, not a reusable credential cache or a lease. The
/// runtime must enforce account admission separately and use the exact peer,
/// preserving `host` for certificate/SNI verification without re-resolving it.
/// Expired material, or a transport unable to do that, must refuse the dial.
#[derive(Serialize, Zeroize, ZeroizeOnDrop)]
pub struct DialMaterial {
    pub provider_id: String,
    pub revision: u64,
    pub secret_ref: String,
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub peer: String,
    pub expires_at: u64,
    pub username: String,
    pub password: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_binds_domain_path_timestamp_and_exact_body() {
        let secret = b"synthetic-resolver-secret-32-bytes";
        let now = 1_800_000_000;
        let body = b"{}";
        let header = authorization_header(Some(secret), REGISTRY_PATH, now, body).unwrap();
        assert_eq!(
            verify(Some(secret), REGISTRY_PATH, Some(&header), body, now),
            Ok(())
        );
        for (path, body, time) in [
            (RESOLVE_PATH, &b"{}"[..], now),
            (REGISTRY_PATH, &b"{ }"[..], now),
            (REGISTRY_PATH, &b"{}"[..], now + MAX_CLOCK_SKEW_SECS + 1),
            (REGISTRY_PATH, &b"{}"[..], now - MAX_CLOCK_SKEW_SECS - 1),
        ] {
            assert_eq!(
                verify(Some(secret), path, Some(&header), body, time),
                Err(AuthError::Unauthorized)
            );
        }
        let meter = crate::meter_sink::authorization_header(Some(secret), now, body).unwrap();
        assert_eq!(
            verify(Some(secret), REGISTRY_PATH, Some(&meter), body, now),
            Err(AuthError::Unauthorized)
        );
        let renamed = meter.replacen("MeterSink ", "PoolResolver ", 1);
        assert_eq!(
            verify(Some(secret), REGISTRY_PATH, Some(&renamed), body, now),
            Err(AuthError::Unauthorized)
        );
        for key in [None, Some(&b"short"[..])] {
            assert_eq!(
                verify(key, REGISTRY_PATH, Some(&header), body, now),
                Err(AuthError::NotConfigured)
            );
            assert!(authorization_header(key, REGISTRY_PATH, now, body).is_none());
        }
        for header in [
            "",
            "Bearer synthetic",
            "PoolResolver NaN.x",
            "PoolResolver 1800000000.x",
        ] {
            assert_eq!(
                verify(Some(secret), REGISTRY_PATH, Some(header), body, now),
                Err(AuthError::Unauthorized)
            );
        }
        assert!(authorization_header(
            Some(secret),
            REGISTRY_PATH,
            now,
            &vec![0; MAX_REQUEST_BYTES + 1]
        )
        .is_none());
    }
}
