//! Request-scoped preparation of persisted Subsonic API-key authentication.

use crate::handlers::StoreError;
use hmac::{Hmac, KeyInit, Mac};
use nntune_subsonic::{AuthStore, Credentials, Identity};
use serde::{de, Deserialize, Deserializer};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::future::Future;
use subtle::ConstantTimeEq;

#[derive(Clone)]
pub struct SubsonicOwnerRow {
    pub user_id: String,
    pub email: String,
    pub deleting_at: Option<i64>,
    pub subsonic_disabled_at: Option<i64>,
}

#[derive(Clone)]
pub struct SubsonicClientRow {
    pub client_id: String,
    pub user_id: String,
    pub subsonic_username: String,
    pub api_key_hmac: String,
    pub pepper_version: u32,
    pub auth_mode: String,
    pub scope: String,
    pub revoked_at: Option<i64>,
    pub owner: Option<SubsonicOwnerRow>,
}

impl std::fmt::Debug for SubsonicClientRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubsonicClientRow").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for SubsonicOwnerRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubsonicOwnerRow").finish_non_exhaustive()
    }
}

pub trait SubsonicClientLookup {
    fn clients_by_candidates(
        &self,
        pairs: &[(u32, String)],
    ) -> impl Future<Output = Result<Vec<SubsonicClientRow>, StoreError>>;
    fn cas_pepper(
        &self,
        expected: &SubsonicClientRow,
        version: u32,
        hmac: &str,
    ) -> impl Future<Output = Result<Option<SubsonicClientRow>, StoreError>>;
    fn mark_last_used(
        &self,
        row: &SubsonicClientRow,
    ) -> impl Future<Output = Result<bool, StoreError>>;
    fn unrevoked_for_pepper(&self, version: u32) -> impl Future<Output = Result<u64, StoreError>>;
}

pub struct SubsonicAuthAttempt {
    pub ip: String,
}

impl std::fmt::Debug for SubsonicAuthAttempt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubsonicAuthAttempt")
            .finish_non_exhaustive()
    }
}

pub trait SubsonicAuthLimiter {
    fn check_general(
        &self,
        attempt: &SubsonicAuthAttempt,
    ) -> impl Future<Output = Result<bool, StoreError>>;
    fn check_auth_attempt(
        &self,
        attempt: &SubsonicAuthAttempt,
    ) -> impl Future<Output = Result<bool, StoreError>>;
    fn check_credential(&self, id: &str) -> impl Future<Output = Result<bool, StoreError>>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubsonicAuthFault {
    Keyring,
    Configuration,
    Store,
    Limiter,
}

/// A result for exactly one request, never a reusable credential cache.
/// Neither the raw key nor the index pepper is retained for dispatch.
#[derive(Default)]
pub struct PreparedSubsonicAuth {
    authenticated: Option<([u8; 32], Identity)>,
    fault: Option<SubsonicAuthFault>,
}

impl PreparedSubsonicAuth {
    pub fn fault(&self) -> Option<SubsonicAuthFault> {
        self.fault
    }
}

impl std::fmt::Debug for PreparedSubsonicAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedSubsonicAuth")
            .finish_non_exhaustive()
    }
}

impl AuthStore for PreparedSubsonicAuth {
    fn verify_api_key(&self, key: &str) -> Option<Identity> {
        let (fingerprint, identity) = self.authenticated.as_ref()?;
        let presented: [u8; 32] = Sha256::digest(key.as_bytes()).into();
        bool::from(fingerprint.ct_eq(&presented)).then(|| identity.clone())
    }
    fn verify_password(&self, _: &str, _: &str) -> Option<Identity> {
        None
    }
    fn verify_token(&self, _: &str, _: &str, _: &str) -> Option<Identity> {
        None
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexKeyring {
    current_version: u32,
    previous_version: Option<u32>,
    #[serde(deserialize_with = "unique_keys")]
    keys: BTreeMap<String, String>,
}

// serde's struct visitor rejects duplicate top-level members. Map visitors
// normally overwrite duplicates; key material must instead reject them too.
fn unique_keys<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, String>, D::Error> {
    struct Unique;
    impl<'de> de::Visitor<'de> for Unique {
        type Value = BTreeMap<String, String>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a unique version-to-key map")
        }
        fn visit_map<M: de::MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
            let mut keys = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, String>()? {
                if keys.insert(key, value).is_some() {
                    return Err(de::Error::custom("duplicate version"));
                }
            }
            Ok(keys)
        }
    }
    deserializer.deserialize_map(Unique)
}

fn positive_version(version: u32) -> Result<i32, StoreError> {
    i32::try_from(version)
        .ok()
        .filter(|v| *v > 0)
        .ok_or(StoreError)
}

fn canonical_hmac(hmac: &str) -> bool {
    hmac.len() == 64
        && hmac
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl IndexKeyring {
    fn candidates(raw: &str, key: &str) -> Result<Vec<(u32, String)>, StoreError> {
        let ring: Self = serde_json::from_str(raw).map_err(|_| StoreError)?;
        positive_version(ring.current_version)?;
        if let Some(previous) = ring.previous_version {
            positive_version(previous)?;
            if previous == ring.current_version {
                return Err(StoreError);
            }
        }
        let versions: Vec<_> = std::iter::once(ring.current_version)
            .chain(ring.previous_version)
            .collect();
        if ring.keys.len() != versions.len() {
            return Err(StoreError);
        }
        versions
            .into_iter()
            .map(|version| {
                let pepper = ring
                    .keys
                    .get(&version.to_string())
                    .filter(|key| !key.is_empty())
                    .ok_or(StoreError)?;
                let mut mac =
                    Hmac::<Sha256>::new_from_slice(pepper.as_bytes()).map_err(|_| StoreError)?;
                mac.update(key.as_bytes());
                let hmac = mac
                    .finalize()
                    .into_bytes()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                Ok((version, hmac))
            })
            .collect()
    }
}

fn eligible(row: &SubsonicClientRow, pairs: &[(u32, String)]) -> bool {
    !row.client_id.is_empty()
        && !row.user_id.is_empty()
        && !row.subsonic_username.is_empty()
        && row.revoked_at.is_none()
        && row.auth_mode == "apikey"
        && row.scope == "subsonic"
        && canonical_hmac(&row.api_key_hmac)
        && pairs.iter().any(|(version, hmac)| {
            *version == row.pepper_version
                && bool::from(row.api_key_hmac.as_bytes().ct_eq(hmac.as_bytes()))
        })
        && row.owner.as_ref().is_some_and(|owner| {
            owner.user_id == row.user_id
                && owner.deleting_at.is_none()
                && owner.subsonic_disabled_at.is_none()
        })
}

fn single_eligible(
    mut rows: Vec<SubsonicClientRow>,
    pairs: &[(u32, String)],
) -> Option<SubsonicClientRow> {
    // A single credential source and composite uniqueness mean even a second
    // identical record is unexpected. Never select an identity by result order.
    if rows.len() != 1 || !eligible(&rows[0], pairs) {
        return None;
    }
    rows.pop()
}

fn auth_fault(fault: SubsonicAuthFault) -> PreparedSubsonicAuth {
    PreparedSubsonicAuth {
        fault: Some(fault),
        ..PreparedSubsonicAuth::default()
    }
}

async fn resolve<S: SubsonicClientLookup, L: SubsonicAuthLimiter>(
    store: &S,
    limiter: &L,
    attempt: &SubsonicAuthAttempt,
    pairs: &[(u32, String)],
) -> Result<Option<SubsonicClientRow>, SubsonicAuthFault> {
    if !limiter
        .check_general(attempt)
        .await
        .map_err(|_| SubsonicAuthFault::Limiter)?
        || !limiter
            .check_auth_attempt(attempt)
            .await
            .map_err(|_| SubsonicAuthFault::Limiter)?
    {
        return Ok(None);
    }
    let rows = store
        .clients_by_candidates(pairs)
        .await
        .map_err(|_| SubsonicAuthFault::Store)?;
    let Some(mut row) = single_eligible(rows, pairs) else {
        return Ok(None);
    };
    if !limiter
        .check_credential(&row.client_id)
        .await
        .map_err(|_| SubsonicAuthFault::Limiter)?
    {
        return Ok(None);
    }
    let current = &pairs[..1];
    if row.pepper_version != current[0].0 {
        let updated = store
            .cas_pepper(&row, current[0].0, &current[0].1)
            .await
            .map_err(|_| SubsonicAuthFault::Store)?;
        let fresh = match updated {
            Some(row) => Some(row).filter(|r| eligible(r, current)),
            None => single_eligible(
                store
                    .clients_by_candidates(current)
                    .await
                    .map_err(|_| SubsonicAuthFault::Store)?,
                current,
            ),
        };
        let Some(fresh) =
            fresh.filter(|fresh| fresh.client_id == row.client_id && fresh.user_id == row.user_id)
        else {
            return Ok(None);
        };
        row = fresh;
    }
    if !store
        .mark_last_used(&row)
        .await
        .map_err(|_| SubsonicAuthFault::Store)?
    {
        return Ok(None);
    }
    Ok(Some(row))
}

/// Shared eligibility check used before resolving any runtime bindings.
/// Unsupported/conflicting forms require neither secrets nor persistence.
pub fn subsonic_api_key(credentials: &Credentials) -> Option<&str> {
    let nonempty = |value: &Option<String>| value.as_deref().is_some_and(|v| !v.is_empty());
    if credentials.username.is_some()
        || [&credentials.password, &credentials.token, &credentials.salt]
            .into_iter()
            .any(nonempty)
    {
        return None;
    }
    credentials.api_key.as_deref().filter(|key| !key.is_empty())
}

/// Protocol conflicts/unsupported forms need no persisted lookup. Operational
/// failures stay distinct internally but expose only the dispatcher's generic 44.
pub async fn prepare_subsonic_auth<S: SubsonicClientLookup, L: SubsonicAuthLimiter>(
    store: Option<&S>,
    keyring_json: Option<&str>,
    limiter: Option<&L>,
    attempt: &SubsonicAuthAttempt,
    credentials: &Credentials,
) -> PreparedSubsonicAuth {
    let Some(key) = subsonic_api_key(credentials) else {
        return PreparedSubsonicAuth::default();
    };
    let (Some(store), Some(limiter)) = (store, limiter) else {
        return auth_fault(SubsonicAuthFault::Configuration);
    };
    let Some(pairs) = keyring_json.and_then(|raw| IndexKeyring::candidates(raw, key).ok()) else {
        return auth_fault(SubsonicAuthFault::Keyring);
    };
    match resolve(store, limiter, attempt, &pairs).await {
        Ok(Some(row)) => {
            let owner = row.owner.expect("eligible row has owner");
            PreparedSubsonicAuth {
                authenticated: Some((
                    Sha256::digest(key.as_bytes()).into(),
                    Identity {
                        username: owner.user_id,
                        email: owner.email,
                        is_admin: false,
                    },
                )),
                fault: None,
            }
        }
        Ok(None) => PreparedSubsonicAuth::default(),
        Err(fault) => auth_fault(fault),
    }
}

/// Metadata-only guard; success is not authorization to retire or rotate keys.
pub async fn subsonic_pepper_retirement_allowed<S: SubsonicClientLookup>(
    store: &S,
    version: u32,
) -> Result<bool, StoreError> {
    positive_version(version)?;
    Ok(store.unrevoked_for_pepper(version).await? == 0)
}

#[derive(PartialEq, Eq)]
pub enum SubsonicSqlParam {
    Integer(i32),
    Text(String),
}
impl std::fmt::Debug for SubsonicSqlParam {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SubsonicSqlParam([REDACTED])")
    }
}

pub struct SubsonicSqlQuery {
    pub sql: String,
    pub params: Vec<SubsonicSqlParam>,
}
impl std::fmt::Debug for SubsonicSqlQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubsonicSqlQuery").finish_non_exhaustive()
    }
}

const CLIENT_PROJECTION: &str = "c.id::text AS client_id, c.user_id, c.subsonic_username, \
    c.api_key_hmac, c.pepper_version, c.auth_mode, c.scope, \
    EXTRACT(EPOCH FROM c.revoked_at)::bigint AS revoked_at, \
    u.id AS owner_user_id, u.email AS owner_email, \
    EXTRACT(EPOCH FROM u.deleting_at)::bigint AS deleting_at, \
    EXTRACT(EPOCH FROM u.subsonic_disabled_at)::bigint AS subsonic_disabled_at";

const OWNER_FENCE: &str = "WITH owner_fence AS MATERIALIZED (\
    SELECT id, email, deleting_at, subsonic_disabled_at FROM users \
    WHERE id = $2 AND deleting_at IS NULL AND subsonic_disabled_at IS NULL FOR SHARE)";
const EXACT_ROW: &str = "c.id = $1::text::uuid AND c.user_id = $2 AND c.user_id = u.id \
    AND c.pepper_version = $3 AND c.api_key_hmac = $4 \
    AND c.revoked_at IS NULL AND c.auth_mode = 'apikey' AND c.scope = 'subsonic'";

pub fn subsonic_lookup_query(pairs: &[(u32, String)]) -> Result<SubsonicSqlQuery, StoreError> {
    if pairs.is_empty() || pairs.len() > 2 || (pairs.len() == 2 && pairs[0].0 == pairs[1].0) {
        return Err(StoreError);
    }
    let mut params = Vec::new();
    let mut predicates = Vec::new();
    for (i, (version, hmac)) in pairs.iter().enumerate() {
        if !canonical_hmac(hmac) {
            return Err(StoreError);
        }
        params.push(SubsonicSqlParam::Integer(positive_version(*version)?));
        params.push(SubsonicSqlParam::Text(hmac.clone()));
        predicates.push(format!(
            "(c.pepper_version = ${} AND c.api_key_hmac = ${})",
            2 * i + 1,
            2 * i + 2
        ));
    }
    Ok(SubsonicSqlQuery {
        sql: format!(
            "SELECT {CLIENT_PROJECTION} FROM subsonic_credentials c \
            JOIN users u ON u.id = c.user_id WHERE {}",
            predicates.join(" OR ")
        ),
        params,
    })
}

fn row_params(row: &SubsonicClientRow) -> Result<Vec<SubsonicSqlParam>, StoreError> {
    if !eligible(row, &[(row.pepper_version, row.api_key_hmac.clone())]) {
        return Err(StoreError);
    }
    Ok(vec![
        SubsonicSqlParam::Text(row.client_id.clone()),
        SubsonicSqlParam::Text(row.user_id.clone()),
        SubsonicSqlParam::Integer(positive_version(row.pepper_version)?),
        SubsonicSqlParam::Text(row.api_key_hmac.clone()),
    ])
}

pub fn subsonic_rehmac_query(
    row: &SubsonicClientRow,
    version: u32,
    hmac: &str,
) -> Result<SubsonicSqlQuery, StoreError> {
    if !canonical_hmac(hmac) || version == row.pepper_version {
        return Err(StoreError);
    }
    let mut params = row_params(row)?;
    params.push(SubsonicSqlParam::Integer(positive_version(version)?));
    params.push(SubsonicSqlParam::Text(hmac.into()));
    Ok(SubsonicSqlQuery {
        sql: format!(
            "{OWNER_FENCE} UPDATE subsonic_credentials c \
            SET pepper_version = $5, api_key_hmac = $6 FROM owner_fence u \
            WHERE {EXACT_ROW} RETURNING {CLIENT_PROJECTION}"
        ),
        params,
    })
}

pub fn subsonic_last_used_query(row: &SubsonicClientRow) -> Result<SubsonicSqlQuery, StoreError> {
    Ok(SubsonicSqlQuery {
        sql: format!(
            "{OWNER_FENCE} UPDATE subsonic_credentials c \
            SET last_used_at = now() FROM owner_fence u \
            WHERE {EXACT_ROW} RETURNING c.id::text AS client_id"
        ),
        params: row_params(row)?,
    })
}

pub fn subsonic_unrevoked_pepper_query(version: u32) -> Result<SubsonicSqlQuery, StoreError> {
    Ok(SubsonicSqlQuery {
        sql: "SELECT count(*) FROM subsonic_credentials \
            WHERE pepper_version = $1 AND revoked_at IS NULL"
            .into(),
        params: vec![SubsonicSqlParam::Integer(positive_version(version)?)],
    })
}
