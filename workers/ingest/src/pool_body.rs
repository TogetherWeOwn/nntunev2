//! Pool body call: one [`BodyRequest`] to the user's `NntpPool` Durable
//! Object (stream Worker), answered with the raw article body or a
//! `404` when the provider no longer has the article.
//!
//! Same trust shape as the probe ([`crate::pool_probe`]): the cross-script
//! Durable Object binding, `id_from_name(user_id)`, no public route and no
//! shared secret. The envelope JSON is the stream crate's
//! `body::BodyRequest`; unknown response fields never appear because a
//! `200` body is the article bytes themselves.

use serde::{Deserialize, Serialize};
use worker::{wasm_bindgen::JsValue, Env, Headers, Method, Request, RequestInit};

use crate::pool_probe::POOL_BINDING;
use crate::probe::validate_message_id;
use crate::queue::QueueMsg;

/// Body route on the Durable Object. The host is a placeholder: a stub
/// fetch is delivered to the object and never leaves the runtime.
pub const POOL_BODY_URL: &str = "https://nntp-pool/body";

/// Largest pool reply accepted: the pool's own article cap
/// (`MAX_BODY_ARTICLE_BYTES` in the stream crate). A longer `200` is a
/// contract bug, not bytes to buffer.
pub const MAX_BODY_REPLY_BYTES: usize = 16 << 20;

/// Redelivery delay when the pool gives no `Retry-After`.
pub const DEFAULT_RETRY_SECS: u32 = 5;

/// Longest `Retry-After` honored; the queue's own backoff caps the rest.
pub const MAX_RETRY_SECS: u32 = 300;

/// One segment to fetch (mirrors the stream crate's envelope).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyRequest {
    pub job_id: String,
    pub user_id: String,
    pub idempotency_key: String,
    pub segment_id: String,
}

/// Build one body envelope from the job and a segment id. The id is
/// re-validated here with the same strict rule the pool applies: it is
/// interpolated into `BODY <id>` on the user's provider connection.
pub fn build_body_request(msg: &QueueMsg, segment_id: &str) -> Result<BodyRequest, &'static str> {
    if !validate_message_id(segment_id) {
        return Err("segment ids must be valid message-ids");
    }
    Ok(BodyRequest {
        job_id: msg.job_id.clone(),
        user_id: msg.user_id.clone(),
        idempotency_key: msg.idempotency_key.clone(),
        segment_id: segment_id.to_string(),
    })
}

/// What one body fetch found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BodyFetch {
    /// The raw article body (still transfer-encoded).
    Bytes(Vec<u8>),
    /// The provider answered `430`: the article is gone.
    Missing,
    /// The article exceeds the fetch cap (`413`): a per-article answer,
    /// not a job failure. The caller judges the segment absent (data
    /// for the par2 block count), exactly like a `430`.
    TooLarge,
}

/// Why a body call produced no usable fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolBodyError {
    /// Pool busy or quarantined, provider down or not loaded, or the
    /// call itself failed: redeliver after `after_secs`.
    Retry { after_secs: u32 },
    /// The pool refused the envelope (`400`), or a `200` broke the size
    /// contract. Retrying the same fetch cannot help.
    Rejected,
}

/// Map one pool HTTP answer to a fetch or a retry decision.
pub fn classify_body_reply(
    status: u16,
    retry_after: Option<&str>,
    body: &[u8],
) -> Result<BodyFetch, PoolBodyError> {
    match status {
        200 => {
            if body.len() > MAX_BODY_REPLY_BYTES {
                return Err(PoolBodyError::Rejected);
            }
            Ok(BodyFetch::Bytes(body.to_vec()))
        }
        // Only the pool's own `missing` body means the provider answered
        // `430`. Any other 404 is a route miss (a stream deployment with
        // no `/body` route yet, or a wrong path): counting it as missing
        // data would quarantine every valid job during a version skew.
        404 if body == br#"{"error":"missing"}"# => Ok(BodyFetch::Missing),
        404 => Err(PoolBodyError::Retry {
            after_secs: DEFAULT_RETRY_SECS,
        }),
        413 => Ok(BodyFetch::TooLarge),
        400 => Err(PoolBodyError::Rejected),
        _ => Err(PoolBodyError::Retry {
            after_secs: retry_after
                .and_then(|v| v.trim().parse::<u32>().ok())
                .map_or(DEFAULT_RETRY_SECS, |s| s.clamp(1, MAX_RETRY_SECS)),
        }),
    }
}

/// Send one body request to the user's pool and classify the answer.
pub async fn call_pool_body(env: &Env, req: &BodyRequest) -> Result<BodyFetch, PoolBodyError> {
    let unavailable = PoolBodyError::Retry {
        after_secs: DEFAULT_RETRY_SECS,
    };
    let body = serde_json::to_string(req).map_err(|_| PoolBodyError::Rejected)?;
    let stub = env
        .durable_object(POOL_BINDING)
        .and_then(|ns| ns.id_from_name(&req.user_id)?.get_stub())
        .map_err(|_| unavailable)?;
    let headers = Headers::new();
    headers
        .set("content-type", "application/json")
        .map_err(|_| unavailable)?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_body(Some(JsValue::from_str(&body)));
    let request = Request::new_with_init(POOL_BODY_URL, &init).map_err(|_| unavailable)?;
    let mut response = stub
        .fetch_with_request(request)
        .await
        .map_err(|_| unavailable)?;
    let status = response.status_code();
    let retry_after = response.headers().get("retry-after").ok().flatten();
    let bytes = response.bytes().await.map_err(|_| unavailable)?;
    classify_body_reply(status, retry_after.as_deref(), &bytes)
}
