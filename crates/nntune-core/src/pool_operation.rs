//! Bounded serving-account wire contract shared by stream and ingest.
//! Attribution is returned only with a successful account-owned operation.
//! It is not a caller-selected account, credential, lease or grant.

use std::collections::BTreeMap;

use crate::{pool_channel, segment_identity::valid_segment_id};
use serde::{Deserialize, Serialize};

pub const MAX_BODY_BYTES: usize = 16 << 20;
pub const MAX_STAT_IDS: usize = 500;
pub const INGEST_BODY_PATH: &str = "/pool-body";
pub const INGEST_STAT_PATH: &str = "/pool-stat";

pub fn valid_provider_id(raw: &str) -> bool {
    raw.len() == 36
        && raw.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
        })
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
            Self::Body(id) => valid_segment_id(id),
            Self::Stat(ids) => {
                !ids.is_empty()
                    && ids.len() <= MAX_STAT_IDS
                    && ids.iter().all(|id| valid_segment_id(id))
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
    let Some(provider_id) = provider_id.filter(|id| valid_provider_id(id)) else {
        return unavailable();
    };
    let output = match content_type {
        "application/octet-stream" if body.len() <= MAX_BODY_BYTES => Output::Body(body),
        "application/json"
            if body.len() <= pool_channel::MAX_OPERATION_BYTES
                && body.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'{') =>
        {
            let Ok(stat) = serde_json::from_slice::<StatResponse>(&body) else {
                return unavailable();
            };
            if stat.present.len().saturating_add(stat.missing.len()) > MAX_STAT_IDS
                || !stat
                    .present
                    .iter()
                    .chain(&stat.missing)
                    .all(|id| valid_segment_id(id))
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

/// Verify the exact operation/result shape and STAT occurrence multiset.
pub fn valid_output(operation: &Operation, output: &Output) -> bool {
    match (operation, output) {
        (Operation::Body(_), Output::Body(bytes)) => bytes.len() <= MAX_BODY_BYTES,
        (Operation::Stat(ids), Output::Stat { present, missing }) => {
            if present.len().saturating_add(missing.len()) != ids.len() {
                return false;
            }
            // Each duplicate is probed independently and may observe expiry
            // between commands. Partition occurrences, not unique ID values.
            let mut expected = BTreeMap::new();
            for id in ids {
                *expected.entry(id.as_str()).or_insert(0_usize) += 1;
            }
            for id in present.iter().chain(missing) {
                let Some(count) = expected.get_mut(id.as_str()) else {
                    return false;
                };
                if *count == 0 {
                    return false;
                }
                *count -= 1;
            }
            expected.values().all(|count| *count == 0)
        }
        _ => false,
    }
}
