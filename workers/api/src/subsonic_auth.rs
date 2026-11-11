//! Production-only async Subsonic credential core (ADR 0002/0006).
//! Not a native REST authorizer and not yet mounted by the Worker. The
//! legacy fixture authenticator remains separate; this core never accepts p=.

use std::future::Future;

use nntune_subsonic::auth::{md5_hex, Credentials};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::{
    field_envelope::{self, Context, FieldEnvelope},
    native_rest::{credential_hmac, Pepper},
    rotation::KekSource,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthMode {
    ApiKey,
    Token,
    Both,
}

/// A separate keyring must be loaded from SUBSONIC_INDEX_PEPPER, never
/// from the native REST pepper secret. At most two exact versions are used.
/// Deliberately no Debug: even configuration failures must not expose keys.
pub struct IndexKeyring<'a> {
    current: Pepper<'a>,
    previous: Option<Pepper<'a>>,
}

impl<'a> IndexKeyring<'a> {
    pub fn new(current: Pepper<'a>, previous: Option<Pepper<'a>>) -> Result<Self, AuthError> {
        if current.version == 0 || current.bytes.len() < 32 {
            return Err(AuthError::Unavailable);
        }
        if let Some(p) = &previous {
            if p.version == 0
                || p.version == current.version
                || p.bytes.len() < 32
                || p.bytes == current.bytes
            {
                return Err(AuthError::Unavailable);
            }
        }
        Ok(Self { current, previous })
    }

    /// Retirement is an explicit lifecycle precondition, not a clock/TTL.
    /// The caller must obtain the live count under the rotation authority's
    /// serialization, including dormant unrevoked credentials and backups.
    pub fn can_retire_previous(&self, unrevoked_previous: u64) -> bool {
        self.previous.is_some() && unrevoked_previous == 0
    }
}

/// Selected row snapshot. No Debug: HMAC and encrypted token material are
/// not diagnostics. The adapter resolves owner_active from current account
/// state, not a caller parameter or a stale credential claim.
#[derive(Clone)]
pub struct CredentialRecord {
    pub id: String,
    pub user_id: String,
    pub username: String,
    pub scope: String,
    pub auth_mode: AuthMode,
    pub revoked: bool,
    pub owner_active: bool,
    pub api_key_hmac: Option<String>,
    pub pepper_version: Option<u32>,
    pub token: Option<TokenEnvelope>,
}

#[derive(Clone)]
pub struct TokenEnvelope {
    pub envelope_version: u16,
    pub payload_format: u16,
    pub kek_version: u32,
    pub envelope: FieldEnvelope,
}

/// Cannot be constructed outside this module. This is a compatibility
/// identity, not a cookie session or native REST bearer credential.
#[derive(PartialEq, Eq)]
pub struct Principal {
    user_id: String,
    credential_id: String,
    username: String,
}

impl std::fmt::Debug for Principal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Principal { surface: subsonic }")
    }
}

impl Principal {
    pub fn user_id(&self) -> &str {
        &self.user_id
    }
    pub fn credential_id(&self) -> &str {
        &self.credential_id
    }
    pub fn username(&self) -> &str {
        &self.username
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthError {
    Unsupported,
    Conflicting,
    MissingSalt,
    InvalidKey,
    InvalidToken,
    Unavailable,
}

impl AuthError {
    pub fn subsonic_code(self) -> i64 {
        match self {
            Self::Unsupported => 42,
            Self::Conflicting => 43,
            Self::MissingSalt => 10,
            Self::InvalidKey => 44,
            Self::InvalidToken => 40,
            Self::Unavailable => 0,
        }
    }
}

/// Prepared HMAC replacement for an old-version match. No plaintext secret.
/// Not Debug: callers must not log the replacement digest.
pub struct Reindex {
    pub pepper_version: u32,
    pub api_key_hmac: String,
}

/// Persistence boundary for the production adapter. All queries are
/// parameterized and fail closed; implementations must not scan all rows.
pub trait CredentialStore {
    /// Composite (version, digest) lookup, scoped to Subsonic rows only.
    fn by_key(
        &self,
        pepper_version: u32,
        digest: &str,
    ) -> impl Future<Output = Result<Option<CredentialRecord>, AuthError>>;
    /// UNIQUE generated non-PII username; email lookup is forbidden.
    fn by_username(
        &self,
        username: &str,
    ) -> impl Future<Output = Result<Option<CredentialRecord>, AuthError>>;
    /// Atomically consume the per-credential attempt allowance, with live
    /// scope/revocation/account checks, BEFORE token decryption. Invalid
    /// tokens consume attempts too. No default/no-op admission implementation.
    fn admit_attempt(
        &self,
        selected: &CredentialRecord,
        now: u64,
    ) -> impl Future<Output = Result<bool, AuthError>>;
    /// Conditional single-row completion: recheck the selected owner,
    /// scope, mode, revocation, active account, and credential material. In
    /// the same transaction, update last_used_at and CAS HMAC/version when
    /// reindex is Some. A changed/revoked snapshot, rate denial or failed
    /// commit returns false/error and MUST NOT yield a Principal. Never
    /// overwrite a concurrent rotation or retry with an unchecked UPDATE.
    fn complete(
        &self,
        selected: &CredentialRecord,
        reindex: Option<&Reindex>,
        now: u64,
    ) -> impl Future<Output = Result<bool, AuthError>>;
}

fn valid_username(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn valid_record(record: &CredentialRecord) -> bool {
    !record.id.is_empty()
        && record.id.len() <= 128
        && !record.user_id.is_empty()
        && record.user_id.len() <= 128
        && valid_username(&record.username)
        && record.scope == "subsonic"
        && !record.revoked
        && record.owner_active
        && match record.auth_mode {
            AuthMode::ApiKey => {
                record.api_key_hmac.is_some()
                    && record.pepper_version.is_some()
                    && record.token.is_none()
            }
            AuthMode::Token => {
                record.api_key_hmac.is_none()
                    && record.pepper_version.is_none()
                    && record.token.is_some()
            }
            AuthMode::Both => {
                record.api_key_hmac.is_some()
                    && record.pepper_version.is_some()
                    && record.token.is_some()
            }
        }
}

fn token_context<'a>(user_id: &'a str, credential_id: &'a str) -> Context<'a> {
    Context {
        table: "subsonic_credentials",
        field: "token_secret_encrypted",
        scope: user_id,
        row: credential_id,
        version: 2,
        format: 1,
    }
}

fn verify_token(
    selected: &CredentialRecord,
    presented: &str,
    salt: &str,
    keks: &impl KekSource,
) -> Result<(), AuthError> {
    let stored = selected.token.as_ref().ok_or(AuthError::InvalidToken)?;
    if stored.envelope_version != 2
        || stored.payload_format != 1
        || stored.envelope.ciphertext.len() < 32 + 16
        || stored.envelope.ciphertext.len() > 512 + 16
    {
        return Err(AuthError::InvalidToken);
    }
    let kek = keks.kek(stored.kek_version).ok_or(AuthError::Unavailable)?;
    let secret = Zeroizing::new(
        field_envelope::open(
            kek,
            stored.kek_version,
            &token_context(&selected.user_id, &selected.id),
            &stored.envelope,
        )
        .map_err(|_| AuthError::InvalidToken)?,
    );
    // The provisioning path uses a generated printable secret, not raw
    // arbitrary bytes or a reversible copy of the API key.
    if !(32..=512).contains(&secret.len()) || !secret.iter().all(u8::is_ascii_graphic) {
        return Err(AuthError::InvalidToken);
    }
    let mut salted = Zeroizing::new(Vec::with_capacity(secret.len() + salt.len()));
    salted.extend_from_slice(&secret);
    salted.extend_from_slice(salt.as_bytes());
    let expected = Zeroizing::new(md5_hex(&salted));
    // MD5 is only the mandated compatibility checksum. Constant-time
    // comparison prevents a prefix oracle; accepting hex case changes no bits.
    let normalized = Zeroizing::new(presented.to_ascii_lowercase());
    if expected.as_bytes().ct_eq(normalized.as_bytes()).unwrap_u8() != 1 {
        return Err(AuthError::InvalidToken);
    }
    Ok(())
}

/// Authenticate without cookies, native bearer keys, client-name identity,
/// or fixture-password fallback. The HTTP adapter must reject duplicate
/// credential parameters before constructing Credentials (including p=).
pub async fn authenticate(
    store: &impl CredentialStore,
    credentials: &Credentials,
    peppers: &IndexKeyring<'_>,
    keks: &impl KekSource,
    now: u64,
) -> Result<Principal, AuthError> {
    if credentials.password.is_some() {
        return Err(AuthError::Unsupported);
    }
    let (selected, reindex, failure) = if let Some(key) = &credentials.api_key {
        if credentials.username.is_some()
            || credentials.token.is_some()
            || credentials.salt.is_some()
        {
            return Err(AuthError::Conflicting);
        }
        if key.is_empty() || key.len() > 512 {
            return Err(AuthError::InvalidKey);
        }
        let mut matched = None;
        for pepper in std::iter::once(&peppers.current).chain(peppers.previous.as_ref()) {
            let digest = Zeroizing::new(credential_hmac(key, pepper.bytes));
            let Some(record) = store.by_key(pepper.version, &digest).await? else {
                continue;
            };
            if !valid_record(&record)
                || record.auth_mode == AuthMode::Token
                || record.pepper_version != Some(pepper.version)
                || record.api_key_hmac.as_ref().is_none_or(|stored| {
                    stored.as_bytes().ct_eq(digest.as_bytes()).unwrap_u8() != 1
                })
            {
                return Err(AuthError::InvalidKey);
            }
            let reindex = (pepper.version != peppers.current.version).then(|| Reindex {
                pepper_version: peppers.current.version,
                api_key_hmac: credential_hmac(key, peppers.current.bytes),
            });
            matched = Some((record, reindex));
            break;
        }
        let (record, reindex) = matched.ok_or(AuthError::InvalidKey)?;
        (record, reindex, AuthError::InvalidKey)
    } else {
        let username = credentials
            .username
            .as_deref()
            .ok_or(AuthError::InvalidToken)?;
        let token = credentials
            .token
            .as_deref()
            .ok_or(AuthError::InvalidToken)?;
        if !valid_username(username)
            || token.len() != 32
            || !token.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(AuthError::InvalidToken);
        }
        let salt = credentials
            .salt
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or(AuthError::MissingSalt)?;
        if salt.len() > 256 {
            return Err(AuthError::InvalidToken);
        }
        let record = store
            .by_username(username)
            .await?
            .ok_or(AuthError::InvalidToken)?;
        if !valid_record(&record)
            || record.auth_mode == AuthMode::ApiKey
            || record.username != username
        {
            return Err(AuthError::InvalidToken);
        }
        // Reserve before any cryptography involving a reversible secret.
        if !store.admit_attempt(&record, now).await? {
            return Err(AuthError::InvalidToken);
        }
        verify_token(&record, token, salt, keks)?;
        (record, None, AuthError::InvalidToken)
    };
    if failure == AuthError::InvalidKey && !store.admit_attempt(&selected, now).await? {
        return Err(failure);
    }
    if !store.complete(&selected, reindex.as_ref(), now).await? {
        return Err(failure);
    }
    Ok(Principal {
        user_id: selected.user_id,
        credential_id: selected.id,
        username: selected.username,
    })
}

#[cfg(test)]
#[path = "subsonic_auth_tests.rs"]
mod tests;
