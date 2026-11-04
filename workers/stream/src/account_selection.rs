//! Bounded account selection/client composition, not a product runtime entry.
//! Hints rank one operation attempt; only that account's owner can admit it.
//! There is no retry/failover after an operation RPC (including a lost reply).

use std::future::Future;

use futures_util::{stream, StreamExt};
use nntune_core::pool_channel::{self, Channel};

use crate::account_dispatch::{OperationReply, OperationRequest};
use crate::account_operation::{Operation, OperationError, Served};
use crate::account_pool::{ranked_candidates, with_session_deadline, Candidate, WorkClass};
use crate::account_route::{
    parse_registry, CandidateRequest, ChannelKeys, RegistryError, RegistrySource, REGISTRY_BODY,
};
use nntune_core::pool_operation::valid_output;

pub const HINT_CONCURRENCY: usize = 8;

/// Dedicated account namespace only, named by the supplied account_id; POST
/// exact signed path/body. No public URL fallback, redirects, caching or retries.
/// Bound each call and response I/O. Hint bytes are <=512; BODY <=16 MiB; STAT
/// <=1 MiB, parsed with duplicate/unknown field rejection into OperationReply.
/// A serving identity is mandatory. Failures expose no provider/transport text.
#[allow(async_fn_in_trait)]
pub trait AccountSource {
    async fn candidate(
        &self,
        account_id: &str,
        authorization: &str,
        body: &[u8],
    ) -> Result<Vec<u8>, RegistryError>;

    async fn execute(&self, account_id: &str, authorization: &str, body: &[u8]) -> OperationReply;
}

/// The deadline covers registry, all hint requests and the single operation,
/// starting when first polled; it must be at most SESSION_TTL_MS. The account
/// server independently retains closure authority if the client is cancelled.
/// Class is chosen by the trusted stream/ingest caller, never user input.
pub async fn run(
    registry: &impl RegistrySource,
    accounts: &impl AccountSource,
    keys: ChannelKeys<'_>,
    class: WorkClass,
    operation: Operation,
    now_ms: impl Fn() -> u64,
    deadline: impl Future<Output = ()>,
) -> Result<Served, OperationError> {
    if !operation.valid() {
        return Err(OperationError::Invalid);
    }
    // A missing account channel must not trigger a registry or candidate read.
    let account_key = keys
        .account
        .filter(|key| key.len() >= 32)
        .ok_or(OperationError::Unavailable)?;
    let work = async {
        let authorization = pool_channel::authorization_header(
            keys.resolver,
            pool_channel::REGISTRY_PATH,
            now_ms() / 1000,
            REGISTRY_BODY,
        )
        .ok_or(OperationError::Unavailable)?;
        let raw = registry
            .fetch(&authorization)
            .await
            .map_err(|_| OperationError::Unavailable)?;
        let rows = parse_registry(&raw).map_err(|_| OperationError::Unavailable)?;
        let hints = stream::iter(rows.into_iter().filter(|a| {
            a.enabled && a.can_read && (class == WorkClass::Stream || a.ingest_limit > 0)
        }))
        .map(|row| {
            let clock = &now_ms;
            async move {
                let body = serde_json::to_vec(&CandidateRequest {
                    account_id: row.id.clone(),
                    class,
                })
                .ok()?;
                let authorization = Channel::Account.authorization_header(
                    Some(account_key),
                    pool_channel::CANDIDATE_PATH,
                    clock() / 1000,
                    &body,
                )?;
                let raw = accounts
                    .candidate(&row.id, &authorization, &body)
                    .await
                    .ok()?;
                if raw.len() > pool_channel::MAX_REQUEST_BYTES
                    || raw.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{')
                {
                    return None;
                }
                let hint: Candidate = serde_json::from_slice(&raw).ok()?;
                (hint.id == row.id && hint.tier == row.tier).then_some(hint)
            }
        })
        .buffer_unordered(HINT_CONCURRENCY)
        .filter_map(|hint| async { hint })
        .collect::<Vec<_>>()
        .await;
        let selected = ranked_candidates(hints)
            .into_iter()
            .next()
            .ok_or(OperationError::Unavailable)?;
        let body = serde_json::to_vec(&OperationRequest {
            account_id: selected.id.clone(),
            class,
            operation: operation.clone(),
        })
        .map_err(|_| OperationError::Invalid)?;
        let authorization = Channel::Account
            .authorization_header(
                Some(account_key),
                pool_channel::OPERATION_PATH,
                now_ms() / 1000,
                &body,
            )
            .ok_or(OperationError::Invalid)?;
        // No attempt on a second account, even on refusal, deadline or uncertain
        // reply. This cannot turn credential failures into a pool-wide retry.
        match accounts.execute(&selected.id, &authorization, &body).await {
            OperationReply::Served(served)
                if served.provider_id == selected.id
                    && valid_output(&operation, &served.output) =>
            {
                Ok(served)
            }
            OperationReply::Failed(error) => Err(error),
            _ => Err(OperationError::Unavailable),
        }
    };
    with_session_deadline(work, deadline)
        .await
        .map_err(|_| OperationError::Deadline)?
}
