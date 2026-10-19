//! Pool probe call: one [`ProbeRequest`] to the user's `NntpPool` Durable
//! Object (stream Worker), answered with a [`ProbeOutcome`].
//!
//! The object is reached through the cross-script Durable Object binding
//! [`POOL_BINDING`] and addressed by `id_from_name(user_id)`, so there is no
//! public route and no shared secret: only Workers holding the binding can
//! call it, and the pool refuses envelopes naming another user. The answer
//! is checked against the request ([`check_outcome`]) before the tally may
//! fold it.

use futures_util::StreamExt;
use worker::{wasm_bindgen::JsValue, Env, Headers, Method, Request, RequestInit};

use crate::probe::{ProbeOutcome, ProbeRequest};

/// Durable Object binding to the stream Worker's `NntpPool` class.
pub const POOL_BINDING: &str = "NNTP_POOL";

/// Probe route on the Durable Object. The host is a placeholder: a stub
/// fetch is delivered to the object and never leaves the runtime.
pub const POOL_PROBE_URL: &str = "https://nntp-pool/probe";

/// Redelivery delay when the pool gives no `Retry-After`.
pub const DEFAULT_RETRY_SECS: u32 = 5;

/// Longest `Retry-After` honored; the queue's own backoff caps the rest.
pub const MAX_RETRY_SECS: u32 = 300;

/// Largest pool probe reply accepted: a `ProbeOutcome` is two id lists plus
/// the batch index, so anything larger is a contract bug, not bytes to
/// retain. An oversize body is `Rejected` because more bytes existed, never
/// parsed from a truncated prefix.
pub const MAX_PROBE_REPLY_BYTES: usize = 1024 * 1024;

/// Why a probe call produced no usable outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolProbeError {
    /// Pool busy or quarantined, provider down or not loaded, or the call
    /// itself failed: redeliver after `after_secs`.
    Retry { after_secs: u32 },
    /// The pool refused the envelope (`400`/`413`) or answered something
    /// that does not match it. A contract bug: retrying cannot help.
    Rejected,
}

/// Check a pool answer against its request: the batch index echoes, and
/// `present` + `missing` are exactly the requested ids (same multiset), so a
/// short, padded or foreign answer can never move the tally.
pub fn check_outcome(req: &ProbeRequest, outcome: &ProbeOutcome) -> Result<(), PoolProbeError> {
    if outcome.batch_index != req.batch_index {
        return Err(PoolProbeError::Rejected);
    }
    let mut answered: Vec<&str> = outcome
        .present
        .iter()
        .chain(&outcome.missing)
        .map(String::as_str)
        .collect();
    let mut asked: Vec<&str> = req.segment_ids.iter().map(String::as_str).collect();
    answered.sort_unstable();
    asked.sort_unstable();
    if answered != asked {
        return Err(PoolProbeError::Rejected);
    }
    Ok(())
}

/// Map one pool HTTP answer to an outcome or a retry decision.
pub fn classify_reply(
    req: &ProbeRequest,
    status: u16,
    retry_after: Option<&str>,
    body: &str,
) -> Result<ProbeOutcome, PoolProbeError> {
    match status {
        200 => {
            if body.len() > MAX_PROBE_REPLY_BYTES {
                return Err(PoolProbeError::Rejected);
            }
            let outcome: ProbeOutcome =
                serde_json::from_str(body).map_err(|_| PoolProbeError::Rejected)?;
            check_outcome(req, &outcome)?;
            Ok(outcome)
        }
        400 | 413 => Err(PoolProbeError::Rejected),
        _ => Err(PoolProbeError::Retry {
            after_secs: retry_after
                .and_then(|v| v.trim().parse::<u32>().ok())
                .map_or(DEFAULT_RETRY_SECS, |s| s.clamp(1, MAX_RETRY_SECS)),
        }),
    }
}

/// Longest one pool probe stub call may take, headers plus body sharing one
/// cancellable deadline. Overridable in tests via
/// [`call_pool_probe_with_deadline`]; production callers use
/// [`call_pool_probe`].
///
/// Sized from the batch arithmetic in `queue_consumer`: one probe batch is
/// up to [`crate::probe::MAX_PROBE_SEGMENTS`] (500) sequential `STAT`
/// round-trips on the pool side, nominally 15 ms each (7.5 s per batch).
/// The per-STAT allowance below (50 ms, over 3x nominal) covers a
/// slow-but-healthy provider at 25 ms per `STAT` (12.5 s per batch) with 2x
/// headroom, plus one nominal 5 s dial allowance. The pool path dials via
/// `nntune_core` `WorkerTransport` (per-operation `config.timeout`, default
/// 20 s in `crates/nntune-core/src/nntp.rs`) with the greeting + `MODE
/// READER` + `AUTHINFO` handshake in `NntpClient::connect`; the 5 s is a
/// nominal allowance for that handshake, not the transport timeout. A
/// warm-stale redial fails fast on the first `Io` (closed-while-idle)
/// before the fresh dial, so a single batch allowance plus one dial covers
/// the rerun path too. The total (30 s) equals [`crate::queue_consumer`]
/// `PROBE_DELIVERY_BUDGET_MS`, so one hung batch holds at most one
/// delivery budget — the same overshoot the driver already allows for one
/// in-flight batch.
pub const POOL_PROBE_DEADLINE_MS: u64 = 30_000;

/// Per-`STAT` allowance inside [`POOL_PROBE_DEADLINE_MS`]: 50 ms covers the
/// 15 ms nominal round-trip with over 3x headroom (25 ms slow-healthy with
/// 2x).
const PROBE_PER_STAT_ALLOWANCE_MS: u64 = 50;

/// One nominal dial allowance inside [`POOL_PROBE_DEADLINE_MS`]: 5 s for the
/// pool handshake (`NntpClient::connect` over `WorkerTransport`; the
/// transport's per-operation `config.timeout` defaults to 20 s).
const PROBE_DIAL_ALLOWANCE_MS: u64 = 5_000;

// Compile-time: the deadline is exactly one full 500-`STAT` batch at the
// per-`STAT` allowance plus one dial, and stays above a slow-healthy batch
// (500 `STAT`s at 25 ms plus one dial = 17.5 s).
const _: () = assert!(
    POOL_PROBE_DEADLINE_MS
        == crate::probe::MAX_PROBE_SEGMENTS as u64 * PROBE_PER_STAT_ALLOWANCE_MS
            + PROBE_DIAL_ALLOWANCE_MS
);
const _: () = assert!(POOL_PROBE_DEADLINE_MS >= 500 * 25 + 5_000);

/// Send one probe batch to the user's pool and check the answer.
pub async fn call_pool_probe(
    env: &Env,
    req: &ProbeRequest,
) -> Result<ProbeOutcome, PoolProbeError> {
    call_pool_probe_with_deadline(env, req, POOL_PROBE_DEADLINE_MS).await
}

/// Probe-call worker behind [`call_pool_probe`] with an explicit per-call
/// deadline, so tests can shrink the bound without waiting the default.
pub async fn call_pool_probe_with_deadline(
    env: &Env,
    req: &ProbeRequest,
    deadline_ms: u64,
) -> Result<ProbeOutcome, PoolProbeError> {
    let unavailable = PoolProbeError::Retry {
        after_secs: DEFAULT_RETRY_SECS,
    };
    let expired = PoolProbeError::Retry {
        after_secs: DEFAULT_RETRY_SECS,
    };
    let body = serde_json::to_string(req).map_err(|_| PoolProbeError::Rejected)?;
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
    let request = Request::new_with_init(POOL_PROBE_URL, &init).map_err(|_| unavailable)?;
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
    // Ceiling for the 200 reply read, bound before any classification so the
    // source pin (cap precedes classify) keeps holding. A non-200 verdict
    // never reads the body: the classifier ignores the body for non-200, so
    // an oversized, non-UTF-8, or unreadable body keeps exactly its
    // status-based verdict (400/413 stay Rejected; a 503 keeps Retry-After).
    let reply_cap = MAX_PROBE_REPLY_BYTES;
    if status != 200 {
        return classify_reply(req, status, retry_after.as_deref(), "");
    }
    // Bounded 200 read under the same deadline: stream the body and fail
    // closed on the chunk that would cross the cap, so no more than the cap
    // is ever retained. A body that is not streamable falls back to one
    // buffered read with the same ceiling. The drain races the shared
    // deadline (partial progress never re-arms it), so headers plus body
    // share one total budget and a stalled body expires instead of hanging.
    let bounded_read = async {
        match response.stream() {
            Ok(stream) => {
                let mut stream = Box::pin(stream);
                let mut buf = Vec::new();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|_| unavailable)?;
                    if buf.len().saturating_add(chunk.len()) > reply_cap {
                        return Err(PoolProbeError::Rejected);
                    }
                    buf.extend_from_slice(&chunk);
                }
                Ok(buf)
            }
            Err(_) => {
                let buf = response.bytes().await.map_err(|_| unavailable)?;
                if buf.len() > reply_cap {
                    return Err(PoolProbeError::Rejected);
                }
                Ok(buf)
            }
        }
    };
    let bounded_read = Box::pin(bounded_read);
    let bytes = crate::pool_deadline::race_against_deadline(bounded_read, &mut deadline).await;
    let bytes = match bytes {
        Some(Ok(bytes)) => bytes,
        Some(Err(read_error)) => return Err(read_error),
        None => return Err(expired),
    };
    drop(deadline);
    let text = String::from_utf8(bytes).map_err(|_| PoolProbeError::Rejected)?;
    classify_reply(req, status, retry_after.as_deref(), &text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::build_upload_dispatch;
    use crate::fixtures::{SMALL_NZB, SMALL_NZB_SEGMENTS};
    use crate::jobs::JobState;
    use crate::probe::{apply_probe_batch, build_probe_request, ProbeTally};

    /// A two-segment probe request for the small fixture NZB.
    fn request() -> ProbeRequest {
        let msg = build_upload_dispatch("user_01", SMALL_NZB).unwrap();
        build_probe_request(&msg, 1, &SMALL_NZB_SEGMENTS).unwrap()
    }

    /// The pool's 200 body for `request()` with the second segment gone, in
    /// the stream side's serialization.
    fn pool_body(req: &ProbeRequest) -> String {
        format!(
            r#"{{"batch_index":1,"present":["{}"],"missing":["{}"]}}"#,
            req.segment_ids[0], req.segment_ids[1]
        )
    }

    #[test]
    fn pool_answer_folds_into_the_tally() {
        let req = request();
        let outcome = classify_reply(&req, 200, None, &pool_body(&req)).expect("valid answer");
        assert_eq!(outcome.missing, vec![req.segment_ids[1].clone()]);

        let mut tally = ProbeTally::new(2);
        let (state, missing) = apply_probe_batch(JobState::Probing, &mut tally, &outcome);
        assert_eq!((state, missing), (JobState::Probing, 1));
    }

    #[test]
    fn request_serializes_to_the_envelope_the_pool_parses() {
        let req = request();
        let json: serde_json::Value = serde_json::to_value(&req).unwrap();
        for field in [
            "job_id",
            "user_id",
            "idempotency_key",
            "batch_index",
            "segment_ids",
        ] {
            assert!(json.get(field).is_some(), "missing {field}");
        }
        assert_eq!(json["batch_index"], 1);
    }

    #[test]
    fn answers_that_do_not_partition_the_request_are_rejected() {
        let req = request();
        let (a, b) = (req.segment_ids[0].clone(), req.segment_ids[1].clone());
        let answer = |batch_index, present: Vec<String>, missing: Vec<String>| ProbeOutcome {
            batch_index,
            present,
            missing,
        };
        assert!(check_outcome(&req, &answer(1, vec![a.clone()], vec![b.clone()])).is_ok());
        assert!(check_outcome(&req, &answer(1, vec![b.clone(), a.clone()], vec![])).is_ok());
        // Wrong batch, short, padded, duplicated or foreign answers.
        let rejected = [
            answer(0, vec![a.clone()], vec![b.clone()]),
            answer(1, vec![a.clone()], vec![]),
            answer(1, vec![a.clone(), b.clone()], vec![b.clone()]),
            answer(1, vec![a.clone(), a.clone()], vec![]),
            answer(1, vec![a.clone()], vec!["other@synthetic.invalid".into()]),
        ];
        for outcome in rejected {
            assert_eq!(
                check_outcome(&req, &outcome),
                Err(PoolProbeError::Rejected),
                "{outcome:?}"
            );
        }
    }

    #[test]
    fn statuses_map_to_retry_or_reject() {
        let req = request();
        assert_eq!(
            classify_reply(&req, 200, None, "not json"),
            Err(PoolProbeError::Rejected)
        );
        assert_eq!(
            classify_reply(&req, 400, None, r#"{"error":"bad_request"}"#),
            Err(PoolProbeError::Rejected)
        );
        assert_eq!(
            classify_reply(&req, 413, None, ""),
            Err(PoolProbeError::Rejected)
        );
        assert_eq!(
            classify_reply(&req, 503, Some("2"), r#"{"error":"busy"}"#),
            Err(PoolProbeError::Retry { after_secs: 2 })
        );
        assert_eq!(
            classify_reply(&req, 503, None, r#"{"error":"provider_unavailable"}"#),
            Err(PoolProbeError::Retry {
                after_secs: DEFAULT_RETRY_SECS
            })
        );
        assert_eq!(
            classify_reply(&req, 502, Some("soon"), ""),
            Err(PoolProbeError::Retry {
                after_secs: DEFAULT_RETRY_SECS
            })
        );
        assert_eq!(
            classify_reply(&req, 503, Some("86400"), ""),
            Err(PoolProbeError::Retry {
                after_secs: MAX_RETRY_SECS
            })
        );
        assert_eq!(
            classify_reply(&req, 503, Some("0"), ""),
            Err(PoolProbeError::Retry { after_secs: 1 })
        );
    }
}
