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

/// Send one probe batch to the user's pool and check the answer.
pub async fn call_pool_probe(
    env: &Env,
    req: &ProbeRequest,
) -> Result<ProbeOutcome, PoolProbeError> {
    let unavailable = PoolProbeError::Retry {
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
    let mut response = stub
        .fetch_with_request(request)
        .await
        .map_err(|_| unavailable)?;
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
    // Bounded 200 read: stream the body and fail closed on the chunk that
    // would cross the cap, so no more than the cap is ever retained. A body
    // that is not streamable falls back to one buffered read with the same
    // ceiling.
    let bytes = match response.stream() {
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
            buf
        }
        Err(_) => {
            let buf = response.bytes().await.map_err(|_| unavailable)?;
            if buf.len() > reply_cap {
                return Err(PoolProbeError::Rejected);
            }
            buf
        }
    };
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
