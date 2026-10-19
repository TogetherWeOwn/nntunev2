//! Pool body call: one [`BodyRequest`] to the user's `NntpPool` Durable
//! Object (stream Worker), answered with the raw article body or a
//! `404` when the provider no longer has the article.
//!
//! Same trust shape as the probe ([`crate::pool_probe`]): the cross-script
//! Durable Object binding, `id_from_name(user_id)`, no public route and no
//! shared secret. The envelope JSON is the stream crate's
//! `body::BodyRequest`; unknown response fields never appear because a
//! `200` body is the article bytes themselves.

use futures_util::StreamExt;
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

/// Longest one pool body stub call may take, headers plus body sharing one
/// cancellable deadline. Overridable in tests via
/// [`call_pool_body_with_deadline`]; production callers use
/// [`call_pool_body`].
///
/// Sized separately from the probe: one body call is a single article, not
/// a 500-`STAT` batch. The pool side needs at most one nominal 5 s dial
/// allowance plus one nominal 10 s `BODY` response allowance, with one
/// warm-stale redial (closed-while-idle fails fast on the first `Io`, then
/// a fresh dial) and 5 s of headroom for DO overhead and the 16 MiB reply
/// transfer: 5 + 10 + 5 + 5 = 25 s. The pool path dials via `nntune_core`
/// `WorkerTransport` (per-operation `config.timeout`, default 20 s in
/// `crates/nntune-core/src/nntp.rs`) with the greeting + `MODE READER` +
/// `AUTHINFO` handshake in `NntpClient::connect`; the 5 s / 10 s figures
/// are nominal allowances, not that transport timeout (the playback
/// `UpstreamTimeouts` in `workers/stream/src/nntp.rs` is a different
/// client). Well above a cold dial plus a full-budget `BODY` (15 s) while
/// still bounding a hang.
pub const POOL_BODY_DEADLINE_MS: u64 = 25_000;

/// One nominal dial allowance inside [`POOL_BODY_DEADLINE_MS`]: 5 s for the
/// pool handshake (`NntpClient::connect` over `WorkerTransport`; the
/// transport's per-operation `config.timeout` defaults to 20 s), counted
/// twice: initial dial plus one warm-stale redial.
const BODY_DIAL_ALLOWANCE_MS: u64 = 5_000;

/// One nominal article allowance inside [`POOL_BODY_DEADLINE_MS`]: 10 s for
/// a single `BODY` response on the pool path (nominal, not a transport
/// timeout).
const BODY_READ_BUDGET_MS: u64 = 10_000;

// Compile-time: the deadline is exactly dial + article + redial-dial +
// headroom, and stays above a cold dial plus a full-budget article.
// Compile-time: the deadline is exactly dial + article + redial-dial +
// headroom, and stays above a cold dial plus a full-budget article.
const _: () = assert!(
    POOL_BODY_DEADLINE_MS
        == BODY_DIAL_ALLOWANCE_MS + BODY_READ_BUDGET_MS + BODY_DIAL_ALLOWANCE_MS + 5_000
);
const _: () = assert!(POOL_BODY_DEADLINE_MS >= 5_000 + 10_000);

/// Fold one more streamed reply chunk into the buffer, enforcing the reply cap
/// on the running total. Pure so the streaming policy is executed by unit tests
/// without a live Worker `Env` and pool `Response`: a missing or lying
/// `Content-Length` falls through to the chunk loop, which can never buffer
/// more than `MAX_BODY_REPLY_BYTES`, because the `Rejected` fires on the chunk
/// that would cross it and the excess is never retained. Callers must propagate
/// the `Err`: a silently truncated prefix handed to `classify_body_reply`
/// classifies as `Bytes`, not `Rejected`, so truncation is never a fix here.
fn push_reply_chunk(buf: &mut Vec<u8>, chunk: &[u8]) -> Result<(), PoolBodyError> {
    if buf.len().saturating_add(chunk.len()) > MAX_BODY_REPLY_BYTES {
        return Err(PoolBodyError::Rejected);
    }
    buf.extend_from_slice(chunk);
    Ok(())
}

/// Send one body request to the user's pool and classify the answer.
///
/// The reply body is never buffered past `MAX_BODY_REPLY_BYTES`: a truthful
/// declared length over the cap fails closed before any byte is read, and an
/// undeclared or lying length is stopped on the chunk that would cross the
/// cap. Oversize is `Rejected` because more bytes existed, never a truncated
/// prefix handed to the classifier, and the excess is dropped unretained.
pub async fn call_pool_body(env: &Env, req: &BodyRequest) -> Result<BodyFetch, PoolBodyError> {
    call_pool_body_with_deadline(env, req, POOL_BODY_DEADLINE_MS).await
}

/// Body-call worker behind [`call_pool_body`] with an explicit per-call
/// deadline, so tests can shrink the bound without waiting the default.
pub async fn call_pool_body_with_deadline(
    env: &Env,
    req: &BodyRequest,
    deadline_ms: u64,
) -> Result<BodyFetch, PoolBodyError> {
    let unavailable = PoolBodyError::Retry {
        after_secs: DEFAULT_RETRY_SECS,
    };
    let expired = PoolBodyError::Retry {
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
    // One cancellable deadline spans headers plus the body drain, armed
    // before the stub fetch so slow headers count against the same budget
    // as a stalled body (same shape as the MusicBrainz body path).
    // `worker::Delay` only registers its timer on first poll, so racing
    // both futures here polls (arms) the deadline from the start. Dropping
    // the pinned `Delay` releases its timer, and every return below drops
    // it: expiry, transport failure, and success all release the timer.
    let mut deadline = Box::pin(worker::Delay::from(std::time::Duration::from_millis(
        deadline_ms,
    )));
    let fetch = Box::pin(stub.fetch_with_request(request));
    let response = crate::pool_deadline::race_against_deadline(fetch, &mut deadline).await;
    let mut response = match response {
        Some(Ok(response)) => response,
        Some(Err(_)) | None => return Err(expired),
    };
    let status = response.status_code();
    let retry_after = response.headers().get("retry-after").ok().flatten();
    if let Ok(Some(declared)) = response.headers().get("content-length") {
        if let Ok(len) = declared.trim().parse::<usize>() {
            if len > MAX_BODY_REPLY_BYTES {
                return Err(PoolBodyError::Rejected);
            }
        }
    }
    let is_empty = matches!(response.body(), worker::ResponseBody::Empty);
    let bytes = if is_empty {
        Vec::new()
    } else {
        let buffered_len = match response.body() {
            worker::ResponseBody::Body(buffered) => Some(buffered.len()),
            _ => None,
        };
        if let Some(len) = buffered_len {
            if len > MAX_BODY_REPLY_BYTES {
                return Err(PoolBodyError::Rejected);
            }
            match response.body() {
                worker::ResponseBody::Body(buffered) => buffered.clone(),
                _ => Vec::new(),
            }
        } else {
            // Streamed drain under the SAME deadline: each chunk read races
            // the shared budget, so a stalled mid-body stream expires to the
            // same `Retry` as stalled headers (partial progress never re-arms
            // it). The cap fold stays streamed: the crossing chunk is dropped
            // unretained and oversize is `Rejected`, never a truncated prefix
            // handed to the classifier.
            let mut buf = Vec::new();
            let mut stream = response.stream().map_err(|_| unavailable)?;
            loop {
                let body_read = Box::pin(stream.next());
                let next =
                    crate::pool_deadline::race_against_deadline(body_read, &mut deadline).await;
                match next {
                    Some(Some(Ok(chunk))) => push_reply_chunk(&mut buf, &chunk)?,
                    Some(None) => break,
                    Some(Some(Err(_))) => return Err(unavailable),
                    None => return Err(expired),
                }
            }
            buf
        }
    };
    drop(deadline);
    classify_body_reply(status, retry_after.as_deref(), &bytes)
}

#[cfg(test)]
mod reply_chunk_tests {
    //! Executed pins for the streamed cap enforcement (`push_reply_chunk`):
    //! the retention suite pins the fetcher wiring by source, so these tests
    //! pin the drain policy itself at the cap boundary, including the
    //! absent/lying-`Content-Length` path (any declared length falls through to
    //! this fold). Deleting the check, flipping `>` to `>=`, or truncating to
    //! the cap must each fail here.

    use super::*;

    fn chunk(n: usize) -> Vec<u8> {
        vec![0x41u8; n]
    }

    #[test]
    fn streamed_chunks_at_exact_cap_classify_as_bytes() {
        // Multi-chunk body totalling exactly the cap: kept whole, then the
        // classifier must see every byte as `Bytes` (producer allows `==` cap).
        let mut buf = Vec::new();
        let half = MAX_BODY_REPLY_BYTES / 2;
        assert!(push_reply_chunk(&mut buf, &chunk(half)).is_ok());
        assert!(push_reply_chunk(&mut buf, &chunk(MAX_BODY_REPLY_BYTES - half)).is_ok());
        assert_eq!(buf.len(), MAX_BODY_REPLY_BYTES);
        match classify_body_reply(200, None, &buf) {
            Ok(BodyFetch::Bytes(bytes)) => assert_eq!(bytes, buf),
            other => panic!("exactly-at-cap streamed body must pass as Bytes, got {other:?}"),
        }
    }

    #[test]
    fn streamed_chunk_one_past_cap_is_rejected() {
        // Cap+1 spread across chunks: the crossing chunk fails closed, and the
        // excess byte is never retained.
        let mut buf = Vec::new();
        let half = MAX_BODY_REPLY_BYTES / 2;
        assert!(push_reply_chunk(&mut buf, &chunk(half)).is_ok());
        assert!(push_reply_chunk(&mut buf, &chunk(MAX_BODY_REPLY_BYTES - half)).is_ok());
        assert_eq!(
            push_reply_chunk(&mut buf, &[0x41u8]),
            Err(PoolBodyError::Rejected)
        );
        assert_eq!(
            buf.len(),
            MAX_BODY_REPLY_BYTES,
            "the crossing chunk must be dropped, not retained"
        );
    }

    #[test]
    fn single_chunk_over_cap_is_rejected_without_buffering() {
        let mut buf = Vec::new();
        assert_eq!(
            push_reply_chunk(&mut buf, &chunk(MAX_BODY_REPLY_BYTES + 1)),
            Err(PoolBodyError::Rejected)
        );
        assert!(
            buf.is_empty(),
            "an over-cap chunk must buffer nothing, not a truncated prefix"
        );
    }

    #[test]
    fn failed_chunk_leaves_prior_bytes_for_the_rejected_path() {
        // A lying-low (or absent) declared length reaches this fold with no
        // pre-check help: the oversize stream must still end `Rejected`, with
        // only the capped prefix in the buffer and no truncated `Bytes`.
        let mut buf = Vec::new();
        assert!(push_reply_chunk(&mut buf, b"hello").is_ok());
        assert_eq!(
            push_reply_chunk(&mut buf, &chunk(MAX_BODY_REPLY_BYTES)),
            Err(PoolBodyError::Rejected)
        );
        assert_eq!(buf, b"hello", "the failed push must not retain the excess");
    }
}
