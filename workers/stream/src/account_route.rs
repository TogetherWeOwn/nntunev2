//! Authenticated per-account scheduling hints. No remote reservation or dial.
//!
//! Each hint refreshes the registry and reconciles the retained durable owner.
//! A hint is never admission authority: the eventual operation must refresh and
//! reserve again in the owning DO. There is deliberately no public fetch route.

use std::collections::BTreeSet;

use nntune_core::pool_channel::{self, Channel};
use serde::{Deserialize, Serialize};

use crate::account_owner::AccountOwner;
use crate::account_pool::{AccountConfig, Candidate, WorkClass};
use crate::account_supervisor::{AccountStorage, CloseControl};

pub const MAX_REGISTRY_BYTES: usize = 512 * 1024;
pub const MAX_ACCOUNTS: usize = 1000;
pub const REGISTRY_BODY: &[u8] = b"{}";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegistryError;

/// Exactly one POST to the API service binding's registry path with
/// `REGISTRY_BODY` and the supplied Authorization value. Refuse non-200 replies;
/// bound bytes while reading and bound the entire call (including response I/O).
/// No retries, redirects, public URL fetch, caching or credential resolution.
#[allow(async_fn_in_trait)]
pub trait RegistrySource {
    async fn fetch(&self, authorization: &str) -> Result<Vec<u8>, RegistryError>;
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    accounts: Vec<AccountConfig>,
}

pub fn parse_registry(raw: &[u8]) -> Result<Vec<AccountConfig>, RegistryError> {
    if raw.len() > MAX_REGISTRY_BYTES || !is_object(raw) {
        return Err(RegistryError);
    }
    let registry: Registry = serde_json::from_slice(raw).map_err(|_| RegistryError)?;
    if registry.accounts.len() > MAX_ACCOUNTS {
        return Err(RegistryError);
    }
    let mut ids = BTreeSet::new();
    if registry
        .accounts
        .iter()
        .any(|a| !a.valid() || !ids.insert(&a.id))
    {
        return Err(RegistryError);
    }
    // Serde's struct visitor also accepts tuple arrays. This is an object-only
    // wire contract, including each account row; keep duplicate-field rejection
    // in the typed parse above rather than decoding only via Value.
    let shape: serde_json::Value = serde_json::from_slice(raw).map_err(|_| RegistryError)?;
    if !shape["accounts"]
        .as_array()
        .is_some_and(|rows| rows.iter().all(serde_json::Value::is_object))
    {
        return Err(RegistryError);
    }
    Ok(registry.accounts)
}

pub(crate) async fn current_account(
    source: &impl RegistrySource,
    key: Option<&[u8]>,
    account_id: &str,
    now_ms: u64,
) -> Result<AccountConfig, RegistryError> {
    account_snapshot(source, key, account_id, now_ms)
        .await?
        .ok_or(RegistryError)
}

/// Alarm refresh: authenticated absence revokes membership; a failed or invalid
/// registry read does not impersonate a kill switch. Cleanup runs independently.
pub async fn refresh<S: AccountStorage, C: CloseControl>(
    owner: &AccountOwner<S, C>,
    source: &impl RegistrySource,
    key: Option<&[u8]>,
    now_ms: impl Fn() -> u64,
) -> Result<(), RegistryError> {
    match account_snapshot(source, key, owner.account_id(), now_ms()).await? {
        Some(current) => {
            owner
                .lock(current, now_ms())
                .await
                .map_err(|_| RegistryError)?;
        }
        None => owner.registry_missing().await,
    }
    Ok(())
}

pub(crate) async fn account_snapshot(
    source: &impl RegistrySource,
    key: Option<&[u8]>,
    account_id: &str,
    now_ms: u64,
) -> Result<Option<AccountConfig>, RegistryError> {
    let authorization = pool_channel::authorization_header(
        key,
        pool_channel::REGISTRY_PATH,
        now_ms / 1000,
        REGISTRY_BODY,
    )
    .ok_or(RegistryError)?;
    let raw = source.fetch(&authorization).await?;
    Ok(parse_registry(&raw)?
        .into_iter()
        .find(|a| a.id == account_id))
}

fn is_object(raw: &[u8]) -> bool {
    raw.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'{')
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateRequest {
    pub account_id: String,
    pub class: WorkClass,
}

pub struct CandidateCall<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub authorization: Option<&'a str>,
    pub body: &'a [u8],
    pub browser_context: bool,
}

pub struct ChannelKeys<'a> {
    pub account: Option<&'a [u8]>,
    pub resolver: Option<&'a [u8]>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CandidateReply {
    NotFound,
    BadRequest,
    Unavailable,
    Hint(Candidate),
}

pub async fn candidate<S: AccountStorage, C: CloseControl>(
    owner: &AccountOwner<S, C>,
    source: &impl RegistrySource,
    call: CandidateCall<'_>,
    keys: ChannelKeys<'_>,
    now_ms: impl Fn() -> u64,
) -> CandidateReply {
    if call.method != "POST"
        || call.path != pool_channel::CANDIDATE_PATH
        || call.browser_context
        || Channel::Account
            .verify(
                keys.account,
                call.path,
                call.authorization,
                call.body,
                now_ms() / 1000,
            )
            .is_err()
    {
        return CandidateReply::NotFound;
    }
    if !is_object(call.body) {
        return CandidateReply::BadRequest;
    }
    let request = match serde_json::from_slice::<CandidateRequest>(call.body) {
        Ok(request) if request.account_id == owner.account_id() => request,
        _ => return CandidateReply::BadRequest,
    };
    let current = match current_account(source, keys.resolver, owner.account_id(), now_ms()).await {
        Ok(current) => current,
        Err(_) => return CandidateReply::Unavailable,
    };
    // Read completion time, not dispatch time: registry latency must not consume
    // a retained owner's cooldown before it is advanced/reconciled.
    match owner.lock(current, now_ms()).await {
        Ok(guard) => CandidateReply::Hint(guard.candidate(request.class)),
        Err(_) => CandidateReply::Unavailable,
    }
}
