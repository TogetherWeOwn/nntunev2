//! Article body fetch for the ingest pipeline: NNTP `BODY` through the
//! user's pool, on [`Lane::Ingest`].
//!
//! Ingest sends one [`BodyRequest`] per segment to the user's `NntpPool`
//! Durable Object and gets the raw article body back (still yEnc-encoded:
//! decoding and verification are ingest's job), or learns the provider no
//! longer has the article ([`BodyOutcome::Missing`], a `430` — data for the
//! par2 block count, not an error). One segment per call keeps every
//! response bounded by [`MAX_BODY_ARTICLE_BYTES`] and makes the ingest side
//! resumable per file.
//!
//! The envelope, error shape and lease discipline mirror the probe
//! ([`crate::probe`]): ids are re-validated with the strict message-id rule
//! (an id is interpolated into `BODY <id>` on the user's authenticated
//! connection), the lease is on [`Lane::Ingest`] so a fetch never takes the
//! playback slot, a full lane or quarantined pool answers `503` with
//! `Retry-After`, and any provider failure quarantines the pool, drops the
//! connection and answers a generic `502`. Provider text never leaves this
//! module. A warm connection the provider closed while idle is redialed
//! once: `BODY` is idempotent, so the fetch simply reruns.

use std::cell::RefCell;
use std::future::Future;

use nntune_core::nntp::NntpClient;
use nntune_core::transport::{is_article_limit_error, Transport};
use serde::{Deserialize, Serialize};

use crate::pool::{CircuitState, Lane, Pool, PoolError};
use crate::probe::{
    provider_failed, valid_probe_id, ProbeError, LANE_FULL_RETRY_MS, MAX_USER_ID_LEN,
};

/// Largest accepted body-request envelope: one id of up to 250 bytes,
/// JSON-escaped at worst to double length, plus the envelope fields.
pub const MAX_BODY_REQUEST_BYTES: usize = 64 * 1024;

/// Largest article body one fetch hands back. The client already bounds a
/// read by `NntpConfig::max_article_bytes`; this caps the reply the pool
/// buffers and returns, so one hostile segment cannot press the Durable
/// Object's memory. An over-cap article is refused with `413`, a
/// request-specific answer that does not quarantine the pool.
pub const MAX_BODY_ARTICLE_BYTES: usize = 16 << 20;

/// One segment to fetch, as ingest sends it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyRequest {
    pub job_id: String,
    pub user_id: String,
    pub idempotency_key: String,
    pub segment_id: String,
}

/// What one `BODY` fetch found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BodyOutcome {
    /// The raw article body (dot-unstuffed, still transfer-encoded).
    Bytes(Vec<u8>),
    /// The provider answered `430`: the article is gone.
    Missing,
}

/// HTTP answer for one body call: raw bytes on `200`, a small JSON error
/// object otherwise (`{"error": <code>}`; `{"error": "missing"}` on `404`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyReply {
    pub status: u16,
    pub body: Vec<u8>,
    pub content_type: &'static str,
    pub retry_after_secs: Option<u64>,
}

impl From<Result<BodyOutcome, ProbeError>> for BodyReply {
    /// Renders a fetch result as its HTTP answer: the article bytes on
    /// `200`, `404` for a missing article, otherwise the error's status
    /// and generic code.
    fn from(result: Result<BodyOutcome, ProbeError>) -> Self {
        match result {
            Ok(BodyOutcome::Bytes(bytes)) => BodyReply {
                status: 200,
                body: bytes,
                content_type: "application/octet-stream",
                retry_after_secs: None,
            },
            Ok(BodyOutcome::Missing) => BodyReply {
                status: 404,
                body: br#"{"error":"missing"}"#.to_vec(),
                content_type: "application/json",
                retry_after_secs: None,
            },
            Err(error) => BodyReply {
                status: error.status(),
                body: serde_json::json!({ "error": error.code() })
                    .to_string()
                    .into_bytes(),
                content_type: "application/json",
                retry_after_secs: error.retry_after_secs(),
            },
        }
    }
}

/// Parse and validate one body-request body for the pool owned by
/// `pool_user` (same refusal rules as the probe envelope).
pub fn parse_body_request(body: &[u8], pool_user: &str) -> Result<BodyRequest, ProbeError> {
    if body.len() > MAX_BODY_REQUEST_BYTES {
        return Err(ProbeError::TooLarge);
    }
    let req: BodyRequest = serde_json::from_slice(body)
        .map_err(|_| ProbeError::BadRequest("body must be a body request"))?;
    if req.user_id.is_empty() || req.user_id.len() > MAX_USER_ID_LEN {
        return Err(ProbeError::BadRequest("invalid user_id"));
    }
    if req.user_id != pool_user {
        return Err(ProbeError::BadRequest("request is for another user's pool"));
    }
    if !valid_probe_id(&req.segment_id) {
        return Err(ProbeError::BadRequest(
            "segment id must be a valid message-id",
        ));
    }
    Ok(req)
}

/// Why one fetch on a live connection produced no outcome.
enum FetchFail {
    /// Article over [`MAX_BODY_ARTICLE_BYTES`]: connection in sync, provider
    /// healthy — refuse only this fetch (`413`).
    TooLarge,
    /// Article over the *client's* read limit, hit mid-dot-block: the
    /// provider is healthy (refuse only this fetch, `413`), but the
    /// transport has invalidated the connection — the rest of the
    /// dot-block is still on the wire, so it must be dropped, never
    /// pooled for reuse.
    TooLargeUnusable,
    /// The connection died (`Io`): redial once when it was warm.
    Closed,
    /// Auth, protocol or any other provider failure.
    Provider,
}

/// Fetch one article body on `client`: `222` is bytes, `430` is missing.
/// The first-response callback runs as soon as one answer proves the
/// provider is responding (half-open probe release, as in the probe).
async fn fetch_body<T: Transport>(
    client: &mut NntpClient<T>,
    segment_id: &str,
    on_response: impl FnOnce(),
) -> Result<BodyOutcome, FetchFail> {
    let mut out = Vec::new();
    match client.body_to(segment_id, &mut out).await {
        Ok(()) => {
            on_response();
            if out.len() > MAX_BODY_ARTICLE_BYTES {
                return Err(FetchFail::TooLarge);
            }
            Ok(BodyOutcome::Bytes(out))
        }
        Err(nntune_core::Error::ArticleNotFound(_)) => {
            on_response();
            Ok(BodyOutcome::Missing)
        }
        // The client's own article limit, enforced while reading the
        // dot-block (it can bind below the reply cap when the client is
        // configured tighter): a request-specific `413`, never a
        // provider failure against the user's pool.
        Err(nntune_core::Error::Io(e)) if is_article_limit_error(&e) => {
            on_response();
            Err(FetchFail::TooLargeUnusable)
        }
        Err(nntune_core::Error::Io(_)) => Err(FetchFail::Closed),
        Err(_) => Err(FetchFail::Provider),
    }
}

/// Run one validated body fetch on `pool`'s ingest lane. `dial` and `tick`
/// are as for [`run_probe`](crate::probe::run_probe); `dial` is called at
/// most once (none when a warm connection answers).
pub async fn run_body<T, D, F, S>(
    pool: &RefCell<Pool<NntpClient<T>>>,
    mut dial: D,
    tick: S,
    req: &BodyRequest,
) -> Result<BodyOutcome, ProbeError>
where
    T: Transport,
    D: FnMut() -> F,
    F: Future<Output = nntune_core::Result<NntpClient<T>>>,
    S: Fn(&mut Pool<NntpClient<T>>),
{
    let (lease, warm, success_epoch, is_probe) = {
        let mut pool = pool.borrow_mut();
        tick(&mut pool);
        let lease = pool.try_acquire(Lane::Ingest).map_err(|e| match e {
            PoolError::Quarantined { retry_after_ms } => ProbeError::Busy { retry_after_ms },
            PoolError::Exhausted => ProbeError::Busy {
                retry_after_ms: LANE_FULL_RETRY_MS,
            },
        })?;
        let warm = pool.take_idle();
        (
            lease,
            warm,
            pool.provider_error_epoch(),
            pool.circuit_state() == CircuitState::HalfOpen,
        )
    };

    if let Some(mut client) = warm {
        let lease_id = lease.id;
        let outcome = fetch_body(&mut client, &req.segment_id, || {
            if is_probe {
                pool.borrow_mut()
                    .record_success_for_if_epoch_matches(lease_id, success_epoch);
            }
        })
        .await;
        match outcome {
            Ok(found) => {
                let mut pool = pool.borrow_mut();
                pool.record_reuse();
                pool.record_success_for_if_epoch_matches(lease.id, success_epoch);
                pool.release(lease, Some(client));
                return Ok(found);
            }
            // Article over the reply cap: the connection is in sync and the
            // provider is healthy, so keep both and refuse only this fetch.
            Err(FetchFail::TooLarge) => {
                let mut pool = pool.borrow_mut();
                pool.record_reuse();
                pool.record_success_for_if_epoch_matches(lease.id, success_epoch);
                pool.release(lease, Some(client));
                return Err(ProbeError::TooLarge);
            }
            // Over the client limit mid-block: provider healthy, so no
            // quarantine and no redial (the same article would refuse
            // again) — but the connection is desynced, so release the
            // lease without pooling it.
            Err(FetchFail::TooLargeUnusable) => {
                let mut pool = pool.borrow_mut();
                pool.record_success_for_if_epoch_matches(lease.id, success_epoch);
                pool.release(lease, None);
                return Err(ProbeError::TooLarge);
            }
            // Closed while idle: drop it and redial once below.
            Err(FetchFail::Closed) => {}
            Err(FetchFail::Provider) => return Err(provider_failed(pool, &tick, lease)),
        }
    }

    let mut client = match dial().await {
        Ok(client) => client,
        Err(_) => return Err(provider_failed(pool, &tick, lease)),
    };
    pool.borrow_mut().record_connect();
    let lease_id = lease.id;
    match fetch_body(&mut client, &req.segment_id, || {
        if is_probe {
            pool.borrow_mut()
                .record_success_for_if_epoch_matches(lease_id, success_epoch);
        }
    })
    .await
    {
        Ok(found) => {
            let mut pool = pool.borrow_mut();
            pool.record_success_for_if_epoch_matches(lease.id, success_epoch);
            pool.release(lease, Some(client));
            Ok(found)
        }
        Err(FetchFail::TooLarge) => {
            let mut pool = pool.borrow_mut();
            pool.record_success_for_if_epoch_matches(lease.id, success_epoch);
            pool.release(lease, Some(client));
            Err(ProbeError::TooLarge)
        }
        Err(FetchFail::TooLargeUnusable) => {
            let mut pool = pool.borrow_mut();
            pool.record_success_for_if_epoch_matches(lease.id, success_epoch);
            pool.release(lease, None);
            Err(ProbeError::TooLarge)
        }
        Err(FetchFail::Closed | FetchFail::Provider) => Err(provider_failed(pool, &tick, lease)),
    }
}

/// Serve one body call end to end: validate `body` for `pool_user`'s pool,
/// then run it (`dial` and `tick` as for [`run_body`]).
pub async fn serve_body<T, D, F, S>(
    pool: &RefCell<Pool<NntpClient<T>>>,
    pool_user: &str,
    body: &[u8],
    dial: D,
    tick: S,
) -> BodyReply
where
    T: Transport,
    D: FnMut() -> F,
    F: Future<Output = nntune_core::Result<NntpClient<T>>>,
    S: Fn(&mut Pool<NntpClient<T>>),
{
    let result = match parse_body_request(body, pool_user) {
        Ok(req) => run_body(pool, dial, tick, &req).await,
        Err(e) => Err(e),
    };
    BodyReply::from(result)
}
