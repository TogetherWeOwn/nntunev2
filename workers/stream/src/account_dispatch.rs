//! Authenticated one-shot account operation envelope. No public Worker route
//! enables this until transport, alarm and staging qualification are complete.
//! Signatures are not leases: every accepted call is admitted by the owner.

use std::future::Future;

use nntune_core::pool_channel::{Channel, OPERATION_PATH};
use serde::{Deserialize, Serialize};

use crate::account_operation::{self, AccountConnector, CleanupScheduler, DialSource, Operation};
use crate::account_owner::AccountOwner;
use crate::account_pool::WorkClass;
use crate::account_route::{CandidateCall, ChannelKeys};
use crate::account_supervisor::AccountStorage;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationRequest {
    pub account_id: String,
    pub class: WorkClass,
    pub operation: Operation,
}

pub use nntune_core::pool_operation::{
    decode_response, OperationReply, OperationResponse, PROVIDER_HEADER,
};

/// Refuse browser context, cross-path replay, unknown/duplicate fields, tuple
/// arrays and invalid IDs before registry/storage/resolver/connector access.
pub fn authorize(
    account_id: &str,
    call: CandidateCall<'_>,
    key: Option<&[u8]>,
    now_ms: u64,
) -> Result<OperationRequest, OperationReply> {
    if call.method != "POST"
        || call.path != OPERATION_PATH
        || call.browser_context
        || Channel::Account
            .verify(key, call.path, call.authorization, call.body, now_ms / 1000)
            .is_err()
    {
        return Err(OperationReply::NotFound);
    }
    if call.body.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return Err(OperationReply::BadRequest);
    }
    let request: OperationRequest =
        serde_json::from_slice(call.body).map_err(|_| OperationReply::BadRequest)?;
    let shape: serde_json::Value =
        serde_json::from_slice(call.body).map_err(|_| OperationReply::BadRequest)?;
    if request.account_id != account_id
        || !crate::account_pool::uuid_shaped(account_id)
        || !shape["operation"].is_object()
        || !request.operation.valid()
    {
        return Err(OperationReply::BadRequest);
    }
    Ok(request)
}

/// Caller supplies the same <=20-second whole-call deadline as the operation
/// driver. Request I/O must also be bounded outside this function. A cancelled
/// dispatch retains the driver's durable cleanup and completion ownership.
#[allow(clippy::too_many_arguments)]
pub async fn dispatch<S, C, D>(
    owner: &AccountOwner<S, C::Control>,
    source: &impl DialSource,
    connector: &C,
    scheduler: &impl CleanupScheduler,
    call: CandidateCall<'_>,
    keys: ChannelKeys<'_>,
    now_ms: impl Fn() -> u64,
    deadline: D,
) -> OperationReply
where
    S: AccountStorage,
    C: AccountConnector,
    D: Future<Output = ()>,
{
    let request = match authorize(owner.account_id(), call, keys.account, now_ms()) {
        Ok(request) => request,
        Err(reply) => return reply,
    };
    match account_operation::run(
        owner,
        source,
        connector,
        scheduler,
        keys.resolver,
        request.class,
        request.operation,
        now_ms,
        deadline,
    )
    .await
    {
        Ok(served) => OperationReply::Served(served),
        Err(error) => OperationReply::Failed(error),
    }
}
