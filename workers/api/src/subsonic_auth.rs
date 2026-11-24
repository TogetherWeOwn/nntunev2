//! Production-only async Subsonic credential core (ADR 0002/0006).
//! Not a native REST authorizer and not yet mounted by the Worker. The
//! legacy fixture authenticator remains separate; this core never accepts p=.

use std::future::Future;

use nntune_subsonic::auth::Credentials;
use nntune_subsonic::construction_budget::ConstructionBudget;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::{field_envelope::FieldEnvelope, native_rest::Pepper, rotation::KekSource};

#[path = "subsonic_auth_verify.rs"]
mod verify;
use verify::{indexed_digest, verify_token};

#[cfg(all(target_arch = "wasm32", feature = "ledger-wasm-test"))]
pub(crate) use verify::verification_uses_the_carried_charge;

#[cfg(test)]
use {
    crate::{field_envelope, native_rest::credential_hmac},
    nntune_subsonic::auth::md5_hex,
    verify::token_context,
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

/// In production, only authentication constructs this compatibility identity.
/// It is not a cookie session or native REST bearer credential.
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
    #[cfg(all(test, feature = "postgres-tests", not(target_arch = "wasm32")))]
    pub(crate) fn postgres_test_fixture(
        user_id: &str,
        credential_id: &str,
        username: &str,
    ) -> Self {
        Self {
            user_id: user_id.into(),
            credential_id: credential_id.into(),
            username: username.into(),
        }
    }

    #[cfg(all(target_arch = "wasm32", feature = "staging-body-wasm-test"))]
    pub(crate) fn staging_test_fixture() -> Self {
        Self {
            user_id: "fixture-user".into(),
            credential_id: "fixture-credential".into(),
            username: "fixture-name".into(),
        }
    }

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

// The staging session borrows one app client for lookup, admission and
// completion. The ordinary store's connection behavior remains separate.
trait AuthSession {
    fn lookup(
        &mut self,
        version: Option<u32>,
        value: &str,
        budget: Option<&mut ConstructionBudget>,
    ) -> impl Future<Output = Result<Option<CredentialRecord>, AuthError>>;

    fn admit(
        &mut self,
        selected: &CredentialRecord,
        now: u64,
    ) -> impl Future<Output = Result<bool, AuthError>>;

    fn finish(
        &mut self,
        selected: &CredentialRecord,
        reindex: Option<&Reindex>,
        now: u64,
    ) -> impl Future<Output = Result<bool, AuthError>>;
}

impl<S: CredentialStore> AuthSession for &S {
    async fn lookup(
        &mut self,
        version: Option<u32>,
        value: &str,
        budget: Option<&mut ConstructionBudget>,
    ) -> Result<Option<CredentialRecord>, AuthError> {
        if budget.is_some() {
            return Err(AuthError::Unavailable);
        }
        match version {
            Some(version) => self.by_key(version, value).await,
            None => self.by_username(value).await,
        }
    }

    async fn admit(&mut self, selected: &CredentialRecord, now: u64) -> Result<bool, AuthError> {
        self.admit_attempt(selected, now).await
    }

    async fn finish(
        &mut self,
        selected: &CredentialRecord,
        reindex: Option<&Reindex>,
        now: u64,
    ) -> Result<bool, AuthError> {
        self.complete(selected, reindex, now).await
    }
}

struct PgAuthSession<'a, C>(&'a mut C);

impl<C: crate::postgres::PgConnect> AuthSession for PgAuthSession<'_, C> {
    async fn lookup(
        &mut self,
        version: Option<u32>,
        value: &str,
        budget: Option<&mut ConstructionBudget>,
    ) -> Result<Option<CredentialRecord>, AuthError> {
        crate::worker_entry::load_staging_credential(
            self.0,
            budget.ok_or(AuthError::Unavailable)?,
            version,
            value,
        )
        .await
    }

    async fn admit(&mut self, selected: &CredentialRecord, now: u64) -> Result<bool, AuthError> {
        crate::worker_entry::mutate_staging_credential(self.0, selected, None, now).await
    }

    async fn finish(
        &mut self,
        selected: &CredentialRecord,
        reindex: Option<&Reindex>,
        now: u64,
    ) -> Result<bool, AuthError> {
        crate::worker_entry::mutate_staging_credential(self.0, selected, Some(reindex), now).await
    }
}

/// Authenticate without cookies, native bearer keys, client-name identity,
/// or fixture-password fallback. The HTTP adapter must reject duplicate
/// credential parameters before constructing Credentials (including p=).
pub async fn authenticate(
    mut store: &impl CredentialStore,
    credentials: &Credentials,
    peppers: &IndexKeyring<'_>,
    keks: &impl KekSource,
    now: u64,
) -> Result<Principal, AuthError> {
    authenticate_in(&mut store, credentials, peppers, keks, now, None).await
}

/// Authenticate a staging request on its already admitted app client and
/// construction ledger. This helper neither connects nor mounts a route.
pub async fn authenticate_staging(
    client: &mut impl crate::postgres::PgConnect,
    credentials: &Credentials,
    peppers: &IndexKeyring<'_>,
    keks: &impl KekSource,
    now: u64,
    budget: &mut ConstructionBudget,
) -> Result<Principal, AuthError> {
    authenticate_in(
        &mut PgAuthSession(client),
        credentials,
        peppers,
        keks,
        now,
        Some(budget),
    )
    .await
}

async fn authenticate_in(
    store: &mut impl AuthSession,
    credentials: &Credentials,
    peppers: &IndexKeyring<'_>,
    keks: &impl KekSource,
    now: u64,
    mut budget: Option<&mut ConstructionBudget>,
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
            let digest = Zeroizing::new(indexed_digest(key, pepper.bytes, budget.as_deref_mut())?);
            let Some(record) = store
                .lookup(Some(pepper.version), &digest, budget.as_deref_mut())
                .await?
            else {
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
            let reindex = if pepper.version != peppers.current.version {
                Some(Reindex {
                    pepper_version: peppers.current.version,
                    api_key_hmac: indexed_digest(
                        key,
                        peppers.current.bytes,
                        budget.as_deref_mut(),
                    )?,
                })
            } else {
                None
            };
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
            .lookup(None, username, budget.as_deref_mut())
            .await?
            .ok_or(AuthError::InvalidToken)?;
        if !valid_record(&record)
            || record.auth_mode == AuthMode::ApiKey
            || record.username != username
        {
            return Err(AuthError::InvalidToken);
        }
        // Reserve before any cryptography involving a reversible secret.
        if !store.admit(&record, now).await? {
            return Err(AuthError::InvalidToken);
        }
        verify_token(&record, token, salt, keks, budget)?;
        (record, None, AuthError::InvalidToken)
    };
    if failure == AuthError::InvalidKey && !store.admit(&selected, now).await? {
        return Err(failure);
    }
    if !store.finish(&selected, reindex.as_ref(), now).await? {
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
