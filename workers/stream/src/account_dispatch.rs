//! Authenticated one-shot account operation envelope. No public Worker route
//! enables this until transport, alarm and staging qualification are complete.
//! Signatures are not leases: every accepted call is admitted by the owner.

use std::future::Future;

use nntune_core::pool_channel::{self, Channel, OPERATION_PATH};
use serde::{Deserialize, Serialize};

use crate::account_operation::{
    self, AccountConnector, CleanupScheduler, DialSource, Operation, OperationError, Output, Served,
};
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

#[derive(Debug, PartialEq, Eq)]
pub enum OperationReply {
    NotFound,
    BadRequest,
    Failed(OperationError),
    Served(Served),
}

pub const PROVIDER_HEADER: &str = "x-nntune-pool-provider";

pub struct OperationResponse {
    pub status: u16,
    pub content_type: &'static str,
    pub provider_id: Option<String>,
    pub body: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StatResponse {
    present: Vec<String>,
    missing: Vec<String>,
}

impl OperationReply {
    /// BODY is binary, never a multi-megabyte decimal JSON byte array. Only
    /// success includes the actual serving account; all failures are generic.
    pub fn into_response(self) -> OperationResponse {
        let status = match self {
            Self::NotFound => 404,
            Self::BadRequest | Self::Failed(OperationError::Invalid) => 400,
            Self::Failed(OperationError::Unavailable) => 503,
            Self::Failed(OperationError::Deadline) => 504,
            Self::Failed(OperationError::Missing) => 410,
            Self::Failed(OperationError::TooLarge) => 413,
            Self::Served(Served {
                provider_id,
                output,
            }) => {
                let (content_type, body) = match output {
                    Output::Body(bytes) => ("application/octet-stream", bytes),
                    Output::Stat { present, missing } => {
                        let Ok(body) = serde_json::to_vec(&StatResponse { present, missing })
                        else {
                            return Self::Failed(OperationError::Unavailable).into_response();
                        };
                        ("application/json", body)
                    }
                };
                return OperationResponse {
                    status: 200,
                    content_type,
                    provider_id: Some(provider_id),
                    body,
                };
            }
        };
        OperationResponse {
            status,
            content_type: "application/json",
            provider_id: None,
            body: br#"{"error":{"code":"account_operation_failed"}}"#.to_vec(),
        }
    }
}

/// Bound bytes WHILE reading in the transport, then use this strict decoder.
/// No response body or transport error is exposed as a failure message.
pub fn decode_response(
    status: u16,
    content_type: &str,
    provider_id: Option<&str>,
    body: Vec<u8>,
) -> OperationReply {
    let unavailable = || OperationReply::Failed(OperationError::Unavailable);
    if status != 200 {
        return match status {
            400 => OperationReply::BadRequest,
            404 => OperationReply::NotFound,
            410 => OperationReply::Failed(OperationError::Missing),
            413 => OperationReply::Failed(OperationError::TooLarge),
            504 => OperationReply::Failed(OperationError::Deadline),
            _ => unavailable(),
        };
    }
    let Some(provider_id) = provider_id.filter(|id| crate::account_pool::uuid_shaped(id)) else {
        return unavailable();
    };
    let output = match content_type {
        "application/octet-stream" if body.len() <= crate::body::MAX_BODY_ARTICLE_BYTES => {
            Output::Body(body)
        }
        "application/json"
            if body.len() <= pool_channel::MAX_OPERATION_BYTES
                && body.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'{') =>
        {
            let Ok(stat) = serde_json::from_slice::<StatResponse>(&body) else {
                return unavailable();
            };
            if stat.present.len().saturating_add(stat.missing.len())
                > crate::probe::MAX_PROBE_SEGMENTS
                || !stat
                    .present
                    .iter()
                    .chain(&stat.missing)
                    .all(|id| crate::probe::valid_probe_id(id))
            {
                return unavailable();
            }
            Output::Stat {
                present: stat.present,
                missing: stat.missing,
            }
        }
        _ => return unavailable(),
    };
    OperationReply::Served(Served {
        provider_id: provider_id.into(),
        output,
    })
}

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
