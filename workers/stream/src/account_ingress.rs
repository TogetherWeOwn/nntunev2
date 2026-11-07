//! Binding-only ingest forwarding. The per-user object checks routing identity,
//! but owns no account, socket or lease. Class is fixed here, not in an envelope.
//! Activation is gated before request I/O by the Worker adapter.

use std::future::Future;

use nntune_core::pool_operation::{
    Operation, OperationError, OperationReply, INGEST_BODY_PATH, INGEST_STAT_PATH,
};

use crate::account_pool::WorkClass;
use crate::account_route::{ChannelKeys, RegistrySource};
use crate::account_selection::{self, AccountSource};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BodyEnvelope {
    #[serde(rename = "job_id")]
    _job_id: String,
    #[serde(rename = "user_id")]
    _user_id: String,
    #[serde(rename = "idempotency_key")]
    _idempotency_key: String,
    #[serde(rename = "segment_id")]
    _segment_id: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StatEnvelope {
    #[serde(rename = "job_id")]
    _job_id: String,
    #[serde(rename = "user_id")]
    _user_id: String,
    #[serde(rename = "idempotency_key")]
    _idempotency_key: String,
    #[serde(rename = "batch_index")]
    _batch_index: usize,
    #[serde(rename = "segment_ids")]
    _segment_ids: Vec<String>,
    #[serde(default, rename = "segment_bytes")]
    _segment_bytes: Vec<Option<u64>>,
}

/// Parse existing ingest envelopes, keeping user/job metadata out of the account
/// operation. Refuse unsupported paths and wrong-owner envelopes before reads.
/// The caller must already enforce binding-only access, POST, no browser/query
/// context, byte caps and a whole-call deadline including request body I/O.
pub fn operation(path: &str, body: &[u8], pool_user: &str) -> Result<Operation, OperationError> {
    // Serde struct deserialization also accepts tuples. Ingress accepts only
    // object envelopes, with no duplicate/unknown fields or class/account hint.
    if body.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return Err(OperationError::Invalid);
    }
    let cap = match path {
        INGEST_BODY_PATH => crate::body::MAX_BODY_REQUEST_BYTES,
        INGEST_STAT_PATH => crate::probe::MAX_PROBE_BODY_BYTES,
        _ => return Err(OperationError::Invalid),
    };
    if body.len() > cap {
        return Err(OperationError::Invalid);
    }
    // These strict projections reject duplicate fields; the existing parsers
    // below remain the authority for message IDs, owner and batch limits.
    match path {
        INGEST_BODY_PATH => {
            serde_json::from_slice::<BodyEnvelope>(body).map_err(|_| OperationError::Invalid)?;
        }
        INGEST_STAT_PATH => {
            serde_json::from_slice::<StatEnvelope>(body).map_err(|_| OperationError::Invalid)?;
        }
        _ => unreachable!(),
    }
    let operation = match path {
        INGEST_BODY_PATH => Operation::Body(
            crate::body::parse_body_request(body, pool_user)
                .map_err(|_| OperationError::Invalid)?
                .segment_id,
        ),
        INGEST_STAT_PATH => Operation::Stat(
            crate::probe::parse_request(body, pool_user)
                .map_err(|_| OperationError::Invalid)?
                .segment_ids,
        ),
        _ => return Err(OperationError::Invalid),
    };
    if !operation.valid() {
        return Err(OperationError::Invalid);
    }
    Ok(operation)
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    registry: &impl RegistrySource,
    accounts: &impl AccountSource,
    keys: ChannelKeys<'_>,
    path: &str,
    body: &[u8],
    pool_user: &str,
    now_ms: impl Fn() -> u64,
    deadline: impl Future<Output = ()>,
) -> OperationReply {
    let operation = match operation(path, body, pool_user) {
        Ok(operation) => operation,
        Err(error) => return OperationReply::Failed(error),
    };
    match account_selection::run(
        registry,
        accounts,
        keys,
        WorkClass::Ingest,
        operation,
        now_ms,
        deadline,
    )
    .await
    {
        Ok(served) => OperationReply::Served(served),
        Err(error) => OperationReply::Failed(error),
    }
}
