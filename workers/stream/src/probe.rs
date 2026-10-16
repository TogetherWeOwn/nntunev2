//! Segment availability probe for the ingest pipeline: NNTP `STAT` through
//! the user's pool, on [`Lane::Ingest`].
//!
//! Ingest sends one batch envelope ([`ProbeRequest`], at most
//! [`MAX_PROBE_SEGMENTS`] message-ids) to the user's `NntpPool` Durable
//! Object and gets back which ids the provider has ([`ProbeOutcome`]). The
//! JSON shape is the ingest crate's `probe::ProbeRequest` /
//! `probe::ProbeOutcome`; unknown request fields are ignored so ingest can
//! grow the envelope first.
//!
//! - Ids are re-validated here, the last stop before the wire, with the same
//!   strict message-id rule ingest applies: an id is interpolated into
//!   `STAT <id>` on the user's authenticated connection.
//! - The lease is on [`Lane::Ingest`], so a probe never takes the slot the
//!   pool reserves for playback. A full lane or a quarantined pool answers
//!   `503` with `Retry-After`; the ingest queue redelivers.
//! - `430` marks one id missing. Any other failure (I/O, auth, unexpected
//!   reply) fails the whole batch with a generic `502`, quarantines the pool
//!   and drops the connection. Provider text never leaves this module.
//! - A warm connection the provider closed while idle is redialed once:
//!   `STAT` is idempotent, so the batch simply reruns.
//!
//! The pool sits in a `RefCell` because the Durable Object serves requests
//! concurrently: no borrow is held across an `await`.

use std::cell::{Cell, RefCell};
use std::future::Future;

use nntune_core::nntp::{NntpClient, NntpConfig};
use nntune_core::transport::Transport;
use serde::{Deserialize, Serialize};

use crate::pool::{Lane, Pool, PoolError};

/// The user's provider as the pool dials it: connection settings (vetted by
/// the dial-time SSRF guard) and the effective connection cap
/// ([`effective_cap`](crate::pool::effective_cap) of BYOP and plan caps).
/// Deliberately not `Debug`: the config carries the provider password.
#[derive(Clone)]
pub struct PoolProvider {
    pub config: NntpConfig,
    pub capacity: u32,
}

/// Most message-ids per probe call (the ingest batch size).
pub const MAX_PROBE_SEGMENTS: usize = 500;

/// Largest accepted probe body: 500 ids of up to 512 bytes, each id
/// JSON-escaped at worst to double length, plus the envelope fields.
pub const MAX_PROBE_BODY_BYTES: usize = 1024 * 1024;

/// Longest accepted `user_id` (the ingest rule).
pub const MAX_USER_ID_LEN: usize = 128;

/// `Retry-After` hint when the ingest lane is full (no quarantine timer to
/// quote).
pub const LANE_FULL_RETRY_MS: u64 = 5_000;

/// One batch of segments to check, as ingest sends it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeRequest {
    pub job_id: String,
    pub user_id: String,
    pub idempotency_key: String,
    pub batch_index: usize,
    pub segment_ids: Vec<String>,
}

/// The pool's answer: `present` and `missing` partition the requested ids,
/// in request order; `batch_index` echoes the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeOutcome {
    pub batch_index: usize,
    pub present: Vec<String>,
    pub missing: Vec<String>,
}

/// Why a probe call produced no outcome. Each maps to one HTTP status and a
/// generic error code; none carries provider text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeError {
    /// Malformed envelope: bad JSON, batch size, ids, or a request for
    /// another user's pool.
    BadRequest(&'static str),
    /// Body over [`MAX_PROBE_BODY_BYTES`].
    TooLarge,
    /// Ingest lane full or pool quarantined; retry after the hint.
    Busy { retry_after_ms: u64 },
    /// The user's provider settings could not be loaded.
    ProviderUnavailable,
    /// The provider failed (connection, auth or protocol). The pool is
    /// quarantined and the connection dropped.
    Provider,
}

impl ProbeError {
    /// HTTP status the Durable Object answers with for this error.
    pub fn status(&self) -> u16 {
        match self {
            ProbeError::BadRequest(_) => 400,
            ProbeError::TooLarge => 413,
            ProbeError::Busy { .. } | ProbeError::ProviderUnavailable => 503,
            ProbeError::Provider => 502,
        }
    }

    /// Stable, generic error code for the `{"error": <code>}` body; never
    /// carries provider text.
    pub fn code(&self) -> &'static str {
        match self {
            ProbeError::BadRequest(_) => "bad_request",
            ProbeError::TooLarge => "too_large",
            ProbeError::Busy { .. } => "busy",
            ProbeError::ProviderUnavailable => "provider_unavailable",
            ProbeError::Provider => "provider_error",
        }
    }

    /// Whole seconds for `Retry-After` (at least 1), only for [`ProbeError::Busy`].
    pub fn retry_after_secs(&self) -> Option<u64> {
        match self {
            ProbeError::Busy { retry_after_ms } => Some(retry_after_ms.div_ceil(1_000).max(1)),
            _ => None,
        }
    }
}

/// HTTP answer for one probe call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeReply {
    pub status: u16,
    /// JSON: a [`ProbeOutcome`] on `200`, `{"error": <code>}` otherwise.
    pub body: String,
    pub retry_after_secs: Option<u64>,
}

impl From<Result<ProbeOutcome, ProbeError>> for ProbeReply {
    /// Renders a probe result as its HTTP answer: the outcome JSON on `200`,
    /// otherwise the error's status and generic code.
    fn from(result: Result<ProbeOutcome, ProbeError>) -> Self {
        match result {
            Ok(outcome) => match serde_json::to_string(&outcome) {
                Ok(body) => ProbeReply {
                    status: 200,
                    body,
                    retry_after_secs: None,
                },
                Err(_) => ProbeReply {
                    status: 500,
                    body: r#"{"error":"internal"}"#.to_string(),
                    retry_after_secs: None,
                },
            },
            Err(error) => ProbeReply {
                status: error.status(),
                body: serde_json::json!({ "error": error.code() }).to_string(),
                retry_after_secs: error.retry_after_secs(),
            },
        }
    }
}

/// Strict message-id check, identical to ingest's `validate_message_id`
/// (RFC 5536 `msg-id`, bracketless): 1..=512 bytes of printable ASCII, no
/// `<`/`>`, exactly one `@` with non-empty sides.
pub fn valid_probe_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 512 {
        return false;
    }
    if !id.bytes().all(|b| (0x21..=0x7E).contains(&b)) {
        return false;
    }
    if id.contains('<') || id.contains('>') {
        return false;
    }
    let mut parts = id.split('@');
    matches!((parts.next(), parts.next(), parts.next()), (Some(a), Some(b), None) if !a.is_empty() && !b.is_empty())
}

/// Parse and validate one probe body for the pool owned by `pool_user`.
///
/// The Durable Object id is derived from the user id, so a request whose
/// envelope names anyone else was routed to the wrong pool and is refused
/// before anything is dialed.
pub fn parse_request(body: &[u8], pool_user: &str) -> Result<ProbeRequest, ProbeError> {
    if body.len() > MAX_PROBE_BODY_BYTES {
        return Err(ProbeError::TooLarge);
    }
    let req: ProbeRequest = serde_json::from_slice(body)
        .map_err(|_| ProbeError::BadRequest("body must be a probe request"))?;
    if req.user_id.is_empty() || req.user_id.len() > MAX_USER_ID_LEN {
        return Err(ProbeError::BadRequest("invalid user_id"));
    }
    if req.user_id != pool_user {
        return Err(ProbeError::BadRequest("request is for another user's pool"));
    }
    if req.segment_ids.is_empty() || req.segment_ids.len() > MAX_PROBE_SEGMENTS {
        return Err(ProbeError::BadRequest(
            "probe batch must be 1..=500 segments",
        ));
    }
    if !req.segment_ids.iter().all(|id| valid_probe_id(id)) {
        return Err(ProbeError::BadRequest(
            "segment ids must be valid message-ids",
        ));
    }
    Ok(req)
}

/// `STAT` every id in order on one connection: `223` is present, `430`
/// missing. Any other failure aborts the batch.
pub async fn stat_batch<T: Transport>(
    client: &mut NntpClient<T>,
    ids: &[String],
) -> nntune_core::Result<(Vec<String>, Vec<String>)> {
    let mut present = Vec::new();
    let mut missing = Vec::new();
    for id in ids {
        match client.stat(id).await {
            Ok(_) => present.push(id.clone()),
            Err(nntune_core::Error::ArticleNotFound(_)) => missing.push(id.clone()),
            Err(e) => return Err(e),
        }
    }
    Ok((present, missing))
}

/// Run one validated probe on `pool`'s ingest lane. `dial` opens and
/// authenticates a fresh provider connection; it is called at most once per
/// probe (none when a warm connection answers).
///
/// `tick` brings the pool's clock up to date (the Durable Object passes
/// [`WallClock::sync`] over `Date.now()`). It runs before the lane is leased
/// and again right before a provider failure is recorded, so quarantine
/// backoff counts from the failure, not from the start of a slow dial or
/// batch.
pub async fn run_probe<T, D, F, S>(
    pool: &RefCell<Pool<NntpClient<T>>>,
    mut dial: D,
    tick: S,
    req: &ProbeRequest,
) -> Result<ProbeOutcome, ProbeError>
where
    T: Transport,
    D: FnMut() -> F,
    F: Future<Output = nntune_core::Result<NntpClient<T>>>,
    S: Fn(&mut Pool<NntpClient<T>>),
{
    let (lease, warm, success_epoch) = {
        let mut pool = pool.borrow_mut();
        tick(&mut pool);
        let lease = pool.try_acquire(Lane::Ingest).map_err(|e| match e {
            PoolError::Quarantined { retry_after_ms } => ProbeError::Busy { retry_after_ms },
            PoolError::Exhausted => ProbeError::Busy {
                retry_after_ms: LANE_FULL_RETRY_MS,
            },
        })?;
        let warm = pool.take_idle();
        (lease, warm, pool.provider_error_epoch())
    };

    if let Some(mut client) = warm {
        match stat_batch(&mut client, &req.segment_ids).await {
            Ok(found) => {
                let mut pool = pool.borrow_mut();
                pool.record_reuse();
                pool.record_success_for_if_epoch_matches(lease.id, success_epoch);
                pool.release(lease, Some(client));
                return Ok(outcome(req, found));
            }
            // Closed while idle: drop it and redial once below.
            Err(nntune_core::Error::Io(_)) => {}
            Err(_) => return Err(provider_failed(pool, &tick, lease)),
        }
    }

    let mut client = match dial().await {
        Ok(client) => client,
        Err(_) => return Err(provider_failed(pool, &tick, lease)),
    };
    pool.borrow_mut().record_connect();
    match stat_batch(&mut client, &req.segment_ids).await {
        Ok(found) => {
            let mut pool = pool.borrow_mut();
            pool.record_success_for_if_epoch_matches(lease.id, success_epoch);
            pool.release(lease, Some(client));
            Ok(outcome(req, found))
        }
        Err(_) => Err(provider_failed(pool, &tick, lease)),
    }
}

/// Wraps the partitioned ids in the outcome that echoes the request's batch.
fn outcome(req: &ProbeRequest, (present, missing): (Vec<String>, Vec<String>)) -> ProbeOutcome {
    ProbeOutcome {
        batch_index: req.batch_index,
        present,
        missing,
    }
}

/// Quarantines the pool, drops the leased connection and returns the generic
/// provider error. The clock is ticked first so the backoff starts now.
fn provider_failed<T: Transport>(
    pool: &RefCell<Pool<NntpClient<T>>>,
    tick: &impl Fn(&mut Pool<NntpClient<T>>),
    lease: crate::pool::Lease,
) -> ProbeError {
    let mut pool = pool.borrow_mut();
    tick(&mut pool);
    pool.record_provider_error_for(lease.id);
    pool.release(lease, None);
    ProbeError::Provider
}

/// Serve one probe call end to end: validate `body` for `pool_user`'s pool,
/// then run it (`dial` and `tick` as for [`run_probe`]).
pub async fn serve_probe<T, D, F, S>(
    pool: &RefCell<Pool<NntpClient<T>>>,
    pool_user: &str,
    body: &[u8],
    dial: D,
    tick: S,
) -> ProbeReply
where
    T: Transport,
    D: FnMut() -> F,
    F: Future<Output = nntune_core::Result<NntpClient<T>>>,
    S: Fn(&mut Pool<NntpClient<T>>),
{
    let result = match parse_request(body, pool_user) {
        Ok(req) => run_probe(pool, dial, tick, &req).await,
        Err(e) => Err(e),
    };
    ProbeReply::from(result)
}

/// Drives the pool's virtual clock from wall-clock milliseconds (the
/// Durable Object reads `Date.now()`). A clock that steps backwards advances
/// by zero and is not counted twice when it recovers.
#[derive(Debug, Default)]
pub struct WallClock {
    last_ms: Cell<Option<u64>>,
}

impl WallClock {
    /// Advances `pool` by the wall-clock time since the latest time seen (zero
    /// on the first call or when the clock stepped backwards).
    pub fn sync<C>(&self, pool: &mut Pool<C>, now_ms: u64) {
        let last = self.last_ms.get();
        if let Some(last) = last {
            pool.advance(now_ms.saturating_sub(last));
        }
        self.last_ms
            .set(Some(last.map_or(now_ms, |l| l.max(now_ms))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An ingest-shaped envelope body for `user` carrying `ids`.
    fn body(user: &str, ids: &[&str]) -> Vec<u8> {
        serde_json::json!({
            "job_id": "job_01",
            "user_id": user,
            "idempotency_key": "idem_01",
            "batch_index": 2,
            "segment_ids": ids,
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn probe_ids_follow_the_ingest_message_id_rule() {
        assert!(valid_probe_id("seg-one@synthetic.invalid"));
        assert!(!valid_probe_id(""));
        assert!(!valid_probe_id(&"a".repeat(513)));
        assert!(!valid_probe_id("has space@synthetic.invalid"));
        assert!(!valid_probe_id(
            "x@y>\r\nAUTHINFO USER attacker\r\nQUIT\r\n<z"
        ));
        assert!(!valid_probe_id("x@sy\nthetic.invalid"));
        assert!(!valid_probe_id("x\0@synthetic.invalid"));
        assert!(!valid_probe_id("x@y@z"));
        assert!(!valid_probe_id("@synthetic.invalid"));
        assert!(!valid_probe_id("local@"));
        assert!(!valid_probe_id("<seg@synthetic.invalid>"));
    }

    #[test]
    fn parse_accepts_the_ingest_envelope_including_unknown_fields() {
        // Ingest may add fields (e.g. per-segment byte ranges) before this
        // side reads them; they must not break the probe.
        let json = br#"{"job_id":"job_01","user_id":"user_01","idempotency_key":"idem_01",
            "batch_index":3,"segment_ids":["a@b.invalid"],"segment_bytes":[null]}"#;
        let req = parse_request(json, "user_01").expect("ingest envelope parses");
        assert_eq!(req.batch_index, 3);
        assert_eq!(req.segment_ids, vec!["a@b.invalid".to_string()]);
    }

    #[test]
    fn parse_refuses_bad_envelopes_before_any_dial() {
        let ok = body("user_01", &["a@b.invalid"]);
        assert!(parse_request(&ok, "user_01").is_ok());
        assert_eq!(
            parse_request(&ok, "user_02"),
            Err(ProbeError::BadRequest("request is for another user's pool"))
        );
        assert!(matches!(
            parse_request(b"not json", "user_01"),
            Err(ProbeError::BadRequest(_))
        ));
        assert!(matches!(
            parse_request(&body("user_01", &[]), "user_01"),
            Err(ProbeError::BadRequest(_))
        ));
        let too_many = vec!["a@b.invalid"; MAX_PROBE_SEGMENTS + 1];
        assert!(matches!(
            parse_request(&body("user_01", &too_many), "user_01"),
            Err(ProbeError::BadRequest(_))
        ));
        assert!(matches!(
            parse_request(&body("user_01", &["x@y>\r\nQUIT\r\n<z"]), "user_01"),
            Err(ProbeError::BadRequest(_))
        ));
        let empty_user = body("", &["a@b.invalid"]);
        assert_eq!(
            parse_request(&empty_user, ""),
            Err(ProbeError::BadRequest("invalid user_id"))
        );
        let oversize = vec![b' '; MAX_PROBE_BODY_BYTES + 1];
        assert_eq!(
            parse_request(&oversize, "user_01"),
            Err(ProbeError::TooLarge)
        );
    }

    #[test]
    fn worst_case_valid_batch_fits_the_body_cap() {
        // 500 ids of 512 bytes made of `"`, which JSON doubles to `\"`.
        let id = format!("{}@{}", "\"".repeat(255), "\"".repeat(256));
        assert!(valid_probe_id(&id));
        let ids = vec![id.as_str(); MAX_PROBE_SEGMENTS];
        let json = body("u".repeat(MAX_USER_ID_LEN).as_str(), &ids);
        assert!(json.len() <= MAX_PROBE_BODY_BYTES, "{} bytes", json.len());
        assert!(parse_request(&json, &"u".repeat(MAX_USER_ID_LEN)).is_ok());
    }

    #[test]
    fn errors_map_to_generic_statuses_and_codes() {
        let busy = ProbeError::Busy {
            retry_after_ms: 1_001,
        };
        assert_eq!((busy.status(), busy.code()), (503, "busy"));
        assert_eq!(busy.retry_after_secs(), Some(2));
        assert_eq!(
            ProbeError::Busy { retry_after_ms: 0 }.retry_after_secs(),
            Some(1)
        );
        assert_eq!(ProbeError::Provider.status(), 502);
        assert_eq!(ProbeError::Provider.retry_after_secs(), None);
        assert_eq!(ProbeError::ProviderUnavailable.status(), 503);
        assert_eq!(ProbeError::TooLarge.status(), 413);
        assert_eq!(ProbeError::BadRequest("x").status(), 400);

        let reply = ProbeReply::from(Err(ProbeError::Provider));
        assert_eq!(reply.status, 502);
        assert_eq!(reply.body, r#"{"error":"provider_error"}"#);
        let reply = ProbeReply::from(Err(busy));
        assert_eq!((reply.status, reply.retry_after_secs), (503, Some(2)));
    }

    #[test]
    fn ok_reply_is_the_outcome_json() {
        let outcome = ProbeOutcome {
            batch_index: 1,
            present: vec!["a@b.invalid".into()],
            missing: vec!["c@d.invalid".into()],
        };
        let reply = ProbeReply::from(Ok(outcome.clone()));
        assert_eq!(reply.status, 200);
        let back: ProbeOutcome = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(back, outcome);
    }

    #[test]
    fn wall_clock_advances_by_elapsed_time_and_never_backwards() {
        let mut pool: Pool<u32> = Pool::new(2);
        let clock = WallClock::default();
        clock.sync(&mut pool, 10_000);
        for _ in 0..crate::pool::CIRCUIT_FAILURE_THRESHOLD {
            pool.record_provider_error();
        }
        clock.sync(&mut pool, 10_500);
        assert!(pool.quarantined());
        // A backwards step adds nothing and is not re-counted on recovery.
        clock.sync(&mut pool, 9_000);
        clock.sync(&mut pool, 39_999);
        assert!(
            pool.quarantined(),
            "the 30-second open interval is not over"
        );
        clock.sync(&mut pool, 40_000);
        assert!(!pool.quarantined());
    }
}
