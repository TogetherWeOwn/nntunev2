//! Request-scoped preparation of persisted Subsonic API-key authentication.

use crate::handlers::StoreError;
use hmac::{Hmac, KeyInit, Mac};
use nntune_subsonic::{AuthStore, Credentials, Identity};
use sha2::{Digest, Sha256};
use std::future::Future;
use subtle::ConstantTimeEq;

#[derive(Clone)]
pub struct SubsonicClientRow {
    pub client_id: String,
    pub user_id: String,
    pub api_key_hmac: String,
    pub identity: Identity,
    pub revoked_at: Option<u64>,
    pub owner_disabled: bool,
}

impl std::fmt::Debug for SubsonicClientRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubsonicClientRow")
            .field("revoked", &self.revoked_at.is_some())
            .field("owner_disabled", &self.owner_disabled)
            .finish_non_exhaustive()
    }
}

pub trait SubsonicClientLookup {
    fn client_by_hmac(
        &self,
        hmac: &str,
    ) -> impl Future<Output = Result<Option<SubsonicClientRow>, StoreError>>;
}

/// A result for exactly one request, never a reusable credential cache.
/// Only an ephemeral key fingerprint survives preparation: neither the raw
/// key nor the index pepper is retained for synchronous dispatch.
#[derive(Default)]
pub struct PreparedSubsonicAuth {
    authenticated: Option<([u8; 32], Identity)>,
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

/// Resolve only API-key-only requests; protocol validation still runs in the
/// dispatcher so missing/conflicting/legacy parameters keep their wire codes.
/// Every failure returns a fresh deny-only result, including store failures.
pub async fn prepare_subsonic_auth<L: SubsonicClientLookup>(
    lookup: Option<&L>,
    pepper: Option<&[u8]>,
    credentials: &Credentials,
) -> PreparedSubsonicAuth {
    let denied = PreparedSubsonicAuth::default;
    let (Some(lookup), Some(pepper), Some(key)) = (lookup, pepper, credentials.api_key.as_deref())
    else {
        return denied();
    };
    if pepper.is_empty()
        || key.is_empty()
        || [
            &credentials.username,
            &credentials.password,
            &credentials.token,
            &credentials.salt,
        ]
        .iter()
        .any(|value| value.as_deref().is_some_and(|value| !value.is_empty()))
    {
        return denied();
    }
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(pepper) else {
        return denied();
    };
    mac.update(key.as_bytes());
    let digest = mac.finalize().into_bytes();
    let hmac: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    let Ok(Some(row)) = lookup.client_by_hmac(&hmac).await else {
        return denied();
    };
    if row.revoked_at.is_some()
        || row.owner_disabled
        || row.client_id.is_empty()
        || row.user_id.is_empty()
        || row.identity.username.is_empty()
        || !bool::from(row.api_key_hmac.as_bytes().ct_eq(hmac.as_bytes()))
    {
        return denied();
    }
    PreparedSubsonicAuth {
        authenticated: Some((Sha256::digest(key.as_bytes()).into(), row.identity)),
    }
}
