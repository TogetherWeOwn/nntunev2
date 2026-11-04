//! Account-owned, one-shot BODY/STAT composition. No product route calls this
//! until Worker transport and staging qualification pass. There is no warm
//! client, credential retry, user identity, metering or remote lease handle.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use nntune_core::nntp::{NntpClient, NntpConfig};
use nntune_core::pool_channel::{self, DIAL_MATERIAL_TTL_SECS};
use nntune_core::transport::{is_article_limit_error, Transport};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::account_owner::AccountOwner;
use crate::account_pool::{with_session_deadline, AccountConfig, WorkClass};
use crate::account_route::{current_account, RegistryError, RegistrySource};
use crate::account_supervisor::{AccountStorage, CloseControl, SessionResult};
use crate::body::MAX_BODY_ARTICLE_BYTES;
use crate::probe::{stat_batch, valid_probe_id, MAX_PROBE_SEGMENTS};

pub const MAX_DIAL_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OperationUnavailable;

/// API service binding only. POST exact body/path, no redirects, retries or
/// cache. Bound response bytes and total response I/O time. Refuse non-200;
/// never log the response or an error derived from it.
#[allow(async_fn_in_trait)]
pub trait DialSource: RegistrySource {
    async fn resolve(
        &self,
        authorization: &str,
        body: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, OperationUnavailable>;
}

/// Establish a durable cleanup wake BEFORE any reservation can be dialed.
/// The wake must survive request cancellation and periodically reconcile
/// revocations and drain controls even when registry reads are unavailable.
#[allow(async_fn_in_trait)]
pub trait CleanupScheduler {
    async fn ensure_cleanup(&self) -> Result<(), OperationUnavailable>;
}

/// Literal public peer and separate TLS DNS identity. Implementations MUST NOT
/// re-resolve, fall back to plaintext, bypass certificate verification, send
/// AUTHINFO or retry. Return independent physical-close authority before setup
/// is polled. Native fakes test composition, not the actual Worker transport.
pub trait AccountConnector {
    type Control: CloseControl;
    type Connection: Transport;
    fn open(
        &self,
        peer: SocketAddr,
        tls_name: String,
    ) -> io::Result<(
        Self::Control,
        impl Future<Output = io::Result<Self::Connection>>,
    )>;
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "ids",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Operation {
    Body(String),
    Stat(Vec<String>),
}

impl Operation {
    pub fn valid(&self) -> bool {
        match self {
            Self::Body(id) => valid_probe_id(id),
            Self::Stat(ids) => {
                !ids.is_empty()
                    && ids.len() <= MAX_PROBE_SEGMENTS
                    && ids.iter().all(|id| valid_probe_id(id))
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Output {
    Body(Vec<u8>),
    Stat {
        present: Vec<String>,
        missing: Vec<String>,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub struct Served {
    /// Actual admitted account, not a caller-provided attribution hint.
    pub provider_id: String,
    pub output: Output,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationError {
    Invalid,
    Unavailable,
    Deadline,
    Missing,
    TooLarge,
}

#[derive(Serialize)]
struct ResolveRequest<'a> {
    provider_id: &'a str,
    revision: u64,
    secret_ref: &'a str,
    class: WorkClass,
}

// Deliberately neither Debug nor Clone/Serialize. Original response bytes and
// the parsed credential copies are wiped on every exit, including cancellation.
#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct DialMaterial {
    provider_id: String,
    revision: u64,
    secret_ref: String,
    host: String,
    port: u16,
    tls: bool,
    peer: String,
    expires_at: u64,
    username: String,
    password: String,
}

impl DialMaterial {
    fn parse(
        raw: &[u8],
        current: &AccountConfig,
        now_ms: u64,
    ) -> Result<(Self, SocketAddr, u64), OperationError> {
        let refuse = || OperationError::Unavailable;
        if raw.len() > MAX_DIAL_BYTES
            || raw.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{')
        {
            return Err(refuse());
        }
        let material: Self = serde_json::from_slice(raw).map_err(|_| refuse())?;
        let peer: IpAddr = material.peer.parse().map_err(|_| refuse())?;
        let expires_ms = material.expires_at.checked_mul(1000).ok_or_else(refuse)?;
        // Expiry from a completion-time clock alone must not extend a proof.
        // A backwards sample is also checked against the owner's high-water
        // clock in start and at BOTH AUTHINFO writes.
        if material.provider_id != current.id
            || material.revision != current.revision
            || material.secret_ref != current.secret_ref
            || !material.tls
            || !matches!(material.port, 119 | 563)
            || nntune_ssrf_guard::ip_is_forbidden(&peer)
            || !valid_dns_name(&material.host)
            || now_ms >= expires_ms
            || expires_ms > now_ms.saturating_add(DIAL_MATERIAL_TTL_SECS * 1000)
            || !valid_credential(&material.username, 255)
            || !valid_credential(&material.password, 1024)
        {
            return Err(refuse());
        }
        let address = SocketAddr::new(peer, material.port);
        Ok((material, address, expires_ms))
    }

    fn config(&mut self) -> NntpConfig {
        NntpConfig {
            host: self.host.clone(),
            port: self.port,
            tls: true,
            allow_insecure_plaintext: false,
            username: Some(std::mem::take(&mut self.username)),
            password: Some(std::mem::take(&mut self.password)),
            timeout: Duration::from_secs(3),
            max_pipelined: 1,
            max_article_bytes: MAX_BODY_ARTICLE_BYTES,
        }
    }
}

// NntpConfig is shared with legacy clients and is not itself zeroizing. This
// owner-local wrapper wipes the moved credentials on setup error/cancellation.
struct CredentialConfig(NntpConfig);
impl Drop for CredentialConfig {
    fn drop(&mut self) {
        self.0.username.zeroize();
        self.0.password.zeroize();
    }
}

fn valid_credential(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn valid_dns_name(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host.parse::<IpAddr>().is_err()
        && host
            .strip_suffix('.')
            .unwrap_or(host)
            .split('.')
            .all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label
                        .as_bytes()
                        .first()
                        .is_some_and(u8::is_ascii_alphanumeric)
                    && label
                        .as_bytes()
                        .last()
                        .is_some_and(u8::is_ascii_alphanumeric)
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
}

/// One admitted operation. `deadline` must be a whole-session timer of at most
/// SESSION_TTL_MS starting when this future is first polled; it also bounds
/// registry/resolution/storage waits. The scheduler is the independent backstop
/// if the caller drops this entire future. Close controls bound their own close.
/// No successful output escapes until physical close AND durable retirement.
#[allow(clippy::too_many_arguments)]
pub async fn run<S, C, D>(
    owner: &AccountOwner<S, C::Control>,
    source: &impl DialSource,
    connector: &C,
    scheduler: &impl CleanupScheduler,
    resolver_key: Option<&[u8]>,
    class: WorkClass,
    operation: Operation,
    now_ms: impl Fn() -> u64,
    deadline: D,
) -> Result<Served, OperationError>
where
    S: AccountStorage,
    C: AccountConnector,
    D: Future<Output = ()>,
{
    if !operation.valid() {
        return Err(OperationError::Invalid);
    }
    let session = Cell::new(None);
    let attempted = Cell::new(false);
    let completion = RefCell::new(None);
    let work = async {
        let current = current_account(source, resolver_key, owner.account_id(), now_ms())
            .await
            .map_err(|RegistryError| OperationError::Unavailable)?;
        {
            let mut guard = owner
                .lock(current.clone(), now_ms())
                .await
                .map_err(|_| OperationError::Unavailable)?;
            scheduler
                .ensure_cleanup()
                .await
                .map_err(|_| OperationError::Unavailable)?;
            let id = guard
                .reserve_current(class)
                .await
                .map_err(|_| OperationError::Unavailable)?;
            session.set(Some(id));
            *completion.borrow_mut() = Some(
                guard
                    .completion(id)
                    .map_err(|_| OperationError::Unavailable)?,
            );
        }
        let body = serde_json::to_vec(&ResolveRequest {
            provider_id: &current.id,
            revision: current.revision,
            secret_ref: &current.secret_ref,
            class,
        })
        .map_err(|_| OperationError::Unavailable)?;
        let authorization = pool_channel::authorization_header(
            resolver_key,
            pool_channel::RESOLVE_PATH,
            now_ms() / 1000,
            &body,
        )
        .ok_or(OperationError::Unavailable)?;
        let raw = source
            .resolve(&authorization, &body)
            .await
            .map_err(|_| OperationError::Unavailable)?;
        let (mut material, peer, expires_ms) = DialMaterial::parse(&raw, &current, now_ms())?;
        drop(raw);
        let config = CredentialConfig(material.config());
        let id = session.get().unwrap();
        let setup = owner
            .start(id, expires_ms, now_ms(), || {
                attempted.set(true);
                connector.open(peer, material.host.clone())
            })
            .await
            .map_err(|_| OperationError::Unavailable)?;
        drop(material);
        let auth_refused = Cell::new(false);
        let io = async {
            let transport = setup.await.map_err(nntune_core::Error::Io)?;
            let mut client = NntpClient::connect_with_auth_guard(transport, &config.0, || {
                if owner.auth_permitted(id, expires_ms, now_ms()) {
                    Ok(())
                } else {
                    auth_refused.set(true);
                    Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "account authorization unavailable",
                    ))
                }
            })
            .await?;
            drop(config);
            match &operation {
                Operation::Body(id) => {
                    let mut bytes = Vec::new();
                    client.body_to(id, &mut bytes).await?;
                    Ok(Output::Body(bytes))
                }
                Operation::Stat(ids) => {
                    let (present, missing) = stat_batch(&mut client, ids).await?;
                    Ok(Output::Stat { present, missing })
                }
            }
        }
        .await;
        let (result, output) = classify(io);
        // Synchronous publication into reservation-owned state precedes ANY
        // wait for the mutation lock. Cancellation cannot lose known failures.
        completion
            .borrow()
            .as_ref()
            .unwrap()
            .complete(if auth_refused.get() {
                SessionResult::Aborted
            } else {
                result
            });
        output
    };
    let whole = async {
        let output = work.await;
        if let Some(id) = session.get() {
            if attempted.get() {
                if owner
                    .finish(id, SessionResult::Aborted, now_ms())
                    .await
                    .is_err()
                {
                    return Err(OperationError::Unavailable);
                }
            } else if owner.cancel_unstarted(id).await.is_err() {
                return Err(OperationError::Unavailable);
            }
        }
        output.map(|output| Served {
            provider_id: owner.account_id().to_string(),
            output,
        })
    };
    match with_session_deadline(whole, deadline).await {
        Ok(output) => output,
        Err(_) => {
            // No unbounded post-deadline storage wait. The already-durable wake
            // drains retained controls; capacity cannot escape with this future.
            if let Some(completion) = completion.borrow().as_ref() {
                completion.complete(SessionResult::Aborted);
            }
            Err(OperationError::Deadline)
        }
    }
}

fn classify(
    result: nntune_core::Result<Output>,
) -> (SessionResult, Result<Output, OperationError>) {
    match result {
        Ok(output) => (SessionResult::Success, Ok(output)),
        Err(nntune_core::Error::ArticleNotFound(_)) => {
            (SessionResult::Success, Err(OperationError::Missing))
        }
        Err(nntune_core::Error::Io(ref e)) if is_article_limit_error(e) => {
            (SessionResult::Success, Err(OperationError::TooLarge))
        }
        Err(ref e) if e.is_auth_failure() => {
            (SessionResult::AuthFailure, Err(OperationError::Unavailable))
        }
        Err(_) => (
            SessionResult::ProviderFailure,
            Err(OperationError::Unavailable),
        ),
    }
}
