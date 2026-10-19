//! Repair dispatch: `Verifying` hands off to the Container batch job.
//!
//! par2 verify itself runs serverless; only when blocks are actually missing
//! does the job leave the worker for native-par2 repair in the Container.
//! The call goes through the service binding `REPAIR_CONTAINER` to the
//! no-route repair Worker (`workers/repair`), which streams the staged
//! scratch objects into its Container and streams the outputs back. No
//! par2 set + missing blocks means the job is unrepairable: the caller
//! fails it instead of dispatching.

use std::future::Future;
use std::time::Duration;

use worker::{wasm_bindgen::JsValue, Delay, Env, Headers, Method, Request, RequestInit};

use futures_util::StreamExt;

use nntune_core::par2::scratch::scratch_prefix;

use crate::jobs::MAX_REPAIR_ROUNDS;
use crate::queue::RepairRequest;

/// Service binding name for the repair Worker (wrangler `[[services]]`).
pub const REPAIR_SERVICE_BINDING: &str = "REPAIR_CONTAINER";
/// Repair Worker route that accepts a [`RepairRequest`] JSON body.
pub const REPAIR_ENDPOINT: &str = "/repair";
/// Internal URL for the service-binding fetch (host is a placeholder;
/// the binding routes it).
pub const REPAIR_URL: &str = "https://repair-worker/repair";
/// The repair Worker replies with a small status document
/// (`{status, exit_code, file_count}`; the full per-file result stays
/// in `out/result.json`), so anything larger is not a repair reply
/// we can trust.
const MAX_REPAIR_REPLY_BYTES: usize = 64 << 10;
/// Total service-hop cutoff: the repair front's 600 s DO budget plus 60 s
/// for forwarding and capped reply completion. A single 660 s wait leaves
/// 240 s of the enclosing queue invocation's 900 s contract for other work,
/// NOT a guarantee that a serial multi-message batch fits. Consumer activation
/// must reconcile batching separately (it remains disabled here). The child's
/// 150 s repair exchange excludes staging/output; see workers/repair/README.md
/// for why these outer cutoffs do not cover every resource-permitted round.
pub const REPAIR_CALL_DEADLINE_MS: u64 = 660_000;

/// Fold one more reply chunk into the buffer, enforcing `cap` on the running
/// total. Pure so the streaming policy is unit-tested without a service
/// binding: the chunk that would cross the cap is refused and the capped
/// prefix stays intact, never a truncated body handed to the classifier
/// afterwards.
fn push_repair_chunk(buf: &mut Vec<u8>, chunk: &[u8], cap: usize) -> bool {
    if buf.len().saturating_add(chunk.len()) > cap {
        return false;
    }
    buf.extend_from_slice(chunk);
    true
}

/// Drain a reply chunk stream within `cap`. A chunk-level transport error
/// fails closed to `Retry`, and the chunk that would cross the ceiling fails
/// closed to `Retry` from its length: the remainder is never retained and no
/// truncated prefix escapes for classification.
async fn drain_repair_stream<S, C, E>(mut stream: S, cap: usize) -> Result<Vec<u8>, RepairCall>
where
    S: futures_util::Stream<Item = Result<C, E>> + Unpin,
    C: AsRef<[u8]>,
{
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(_) => return Err(RepairCall::Retry),
        };
        if !push_repair_chunk(&mut buf, chunk.as_ref(), cap) {
            return Err(RepairCall::Retry);
        }
    }
    Ok(buf)
}

/// Decide the `Verifying` handoff: `Ok(Some(_))` dispatches to the repair
/// Worker, `Ok(None)` skips repair, `Err` means unrepairable (fail the job).
/// `round` is the 1-based round about to run (the checkpoint's
/// `repair_rounds` after the `Verifying -> Repairing` advance).
pub fn decide_repair(
    job_id: &str,
    user_id: &str,
    idempotency_key: &str,
    par2_set_id: &str,
    missing_blocks: u32,
    round: u32,
) -> Result<Option<RepairRequest>, &'static str> {
    if missing_blocks == 0 {
        return Ok(None);
    }
    if par2_set_id.is_empty() || par2_set_id.len() > 256 {
        return Err("missing blocks but no par2 set covers them");
    }
    if round == 0 || round > MAX_REPAIR_ROUNDS {
        return Err("repair round outside the paid-round budget");
    }
    let Some(scratch_prefix) = scratch_prefix(job_id, round) else {
        return Err("job id cannot form a scratch prefix");
    };
    Ok(Some(RepairRequest {
        job_id: job_id.to_string(),
        user_id: user_id.to_string(),
        idempotency_key: idempotency_key.to_string(),
        par2_set_id: par2_set_id.to_string(),
        missing_blocks,
        round,
        scratch_prefix,
    }))
}

/// Outcome of one repair call, decided from the reply alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairCall {
    /// Repair finished; re-verify the outputs.
    Repaired,
    /// Terminal for this job: unrepairable set, or a request the repair
    /// Worker rejects as permanently invalid.
    Unrepairable,
    /// Transient (timeout, resource ceilings, transport): retry through
    /// the shared job attempt budget.
    Retry,
}

/// Classify a repair Worker reply. Pure so the mapping is unit-tested
/// without a service binding. Only a well-formed 200 status document
/// can be terminal-success; 4xx rejections are permanent (a retry
/// would send the same bytes), everything else retries.
pub fn classify_repair_reply(status: u16, body: &[u8]) -> RepairCall {
    if body.len() > MAX_REPAIR_REPLY_BYTES {
        return RepairCall::Retry;
    }
    match status {
        200 => {
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
                return RepairCall::Retry;
            };
            match value.get("status").and_then(|s| s.as_str()) {
                Some("repaired") => RepairCall::Repaired,
                Some("unrepairable") => RepairCall::Unrepairable,
                _ => RepairCall::Retry,
            }
        }
        400 | 401 | 403 | 404 | 409 | 413 => RepairCall::Unrepairable,
        _ => RepairCall::Retry,
    }
}

/// Settle headers and the capped reply under one owned, cancellable deadline.
/// The same deadline is polled first at both stages; body progress never
/// replenishes it. Ownership releases the timer on every return (including
/// transport failure). Local settlement does not prove remote cancellation.
/// Generic chunks let the runtime retain its zero-copy HTTP body adapter.
pub async fn settle_repair_call<F, S, D, C, E>(fetch: F, mut deadline: D) -> RepairCall
where
    F: Future<Output = Result<(u16, S), ()>> + Unpin,
    S: futures_util::Stream<Item = Result<C, E>> + Unpin,
    C: AsRef<[u8]>,
    D: Future<Output = ()> + Unpin,
{
    use crate::pool_deadline::race_against_deadline;

    let (status, stream) = match race_against_deadline(fetch, &mut deadline).await {
        Some(Ok(reply)) => reply,
        Some(Err(())) | None => return RepairCall::Retry,
    };
    let drain = Box::pin(drain_repair_stream(stream, MAX_REPAIR_REPLY_BYTES));
    let body = match race_against_deadline(drain, &mut deadline).await {
        Some(Ok(body)) => body,
        Some(Err(_)) | None => return RepairCall::Retry,
    };
    classify_repair_reply(status, &body)
}

/// Dispatch one repair request through the service binding. Transport
/// or binding failures are `Retry`, never a guessed terminal outcome.
///
/// The reply body streams within `MAX_REPAIR_REPLY_BYTES`: the chunk that
/// would cross the ceiling returns `Retry` from the length. The remainder
/// is never retained and no truncated prefix reaches
/// `classify_repair_reply`.
pub async fn call_repair(env: &Env, req: &RepairRequest) -> RepairCall {
    call_repair_with_deadline(env, req, REPAIR_CALL_DEADLINE_MS).await
}

/// Dispatch with an injectable total headers-plus-body budget in milliseconds.
pub async fn call_repair_with_deadline(
    env: &Env,
    req: &RepairRequest,
    deadline_ms: u64,
) -> RepairCall {
    let Ok(body) = serde_json::to_string(req) else {
        return RepairCall::Retry;
    };
    let fetcher = match env.service(REPAIR_SERVICE_BINDING) {
        Ok(fetcher) => fetcher,
        Err(error) => {
            worker::console_log!("repair binding unavailable: {:?}", error);
            return RepairCall::Retry;
        }
    };
    let headers = Headers::new();
    if headers.set("content-type", "application/json").is_err() {
        return RepairCall::Retry;
    }
    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_body(Some(JsValue::from_str(&body)));
    let request = match Request::new_with_init(REPAIR_URL, &init) {
        Ok(request) => request,
        Err(_) => return RepairCall::Retry,
    };
    // Delay registers on first poll: the settlement polls it before the
    // lazy fetch, then reuses it through the capped HTTP body drain. Dropping
    // the owned Delay cancels its local timer, not necessarily remote work.
    let deadline = Box::pin(Delay::from(Duration::from_millis(deadline_ms)));
    let fetch = Box::pin(async {
        let response = fetcher.fetch_request(request).await.map_err(|error| {
            worker::console_log!("repair call failed job={}: {:?}", req.job_id, error);
        })?;
        Ok((response.status().as_u16(), response.into_body()))
    });
    settle_repair_call(fetch, deadline).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const JOB: &str = "job_0123456789abcdef";
    const USER: &str = "user_01";
    const KEY: &str = "ingest:user_01:abc:upload";

    #[test]
    fn missing_blocks_with_par2_dispatches() {
        let req = decide_repair(JOB, USER, KEY, "par2-set-01", 7, 2)
            .unwrap()
            .expect("must dispatch");
        assert_eq!(req.job_id, JOB);
        assert_eq!(req.par2_set_id, "par2-set-01");
        assert_eq!(req.missing_blocks, 7);
        assert_eq!(req.idempotency_key, KEY);
        assert_eq!(req.round, 2);
        assert_eq!(
            req.scratch_prefix,
            "ingest-scratch/job_0123456789abcdef/r2/"
        );
    }

    #[test]
    fn complete_sets_need_no_repair() {
        assert!(decide_repair(JOB, USER, KEY, "par2-set-01", 0, 1)
            .unwrap()
            .is_none());
    }

    #[test]
    fn missing_blocks_without_par2_is_unrepairable() {
        assert!(decide_repair(JOB, USER, KEY, "", 7, 1).is_err());
    }

    #[test]
    fn rounds_outside_the_budget_do_not_dispatch() {
        assert!(decide_repair(JOB, USER, KEY, "par2-set-01", 7, 0).is_err());
        assert!(decide_repair(JOB, USER, KEY, "par2-set-01", 7, MAX_REPAIR_ROUNDS + 1).is_err());
        assert!(decide_repair("a/b", USER, KEY, "par2-set-01", 7, 1).is_err());
    }

    #[test]
    fn repair_reply_classification_is_terminal_or_retry() {
        assert_eq!(
            classify_repair_reply(200, br#"{"status":"repaired","files":[]}"#),
            RepairCall::Repaired
        );
        assert_eq!(
            classify_repair_reply(
                200,
                br#"{"status":"unrepairable","exit_code":1,"files":[]}"#
            ),
            RepairCall::Unrepairable
        );
        assert_eq!(
            classify_repair_reply(503, br#"{"error":"repair_timeout"}"#),
            RepairCall::Retry
        );
        assert_eq!(
            classify_repair_reply(400, br#"{"error":"invalid_request"}"#),
            RepairCall::Unrepairable
        );
        // A malformed or oversized 200 is never a terminal success.
        assert_eq!(classify_repair_reply(200, b"not json"), RepairCall::Retry);
        assert_eq!(classify_repair_reply(200, br#"{}"#), RepairCall::Retry);
        assert_eq!(
            classify_repair_reply(200, &vec![b'x'; MAX_REPAIR_REPLY_BYTES + 1]),
            RepairCall::Retry
        );
        assert_eq!(classify_repair_reply(500, b""), RepairCall::Retry);
    }

    #[test]
    fn status_only_reply_for_max_file_count_classifies() {
        // The repair Worker's wire reply carries no per-file list, so
        // a 1024-file repair still fits the reply cap.
        assert_eq!(
            classify_repair_reply(
                200,
                br#"{"status":"repaired","exit_code":0,"file_count":1024}"#
            ),
            RepairCall::Repaired
        );
        assert_eq!(
            classify_repair_reply(
                200,
                br#"{"status":"unrepairable","exit_code":6,"file_count":0}"#
            ),
            RepairCall::Unrepairable
        );
    }

    fn repaired_body_of_len(total_len: usize) -> Vec<u8> {
        const PREFIX: &[u8] = br#"{"status":"repaired","pad":""#;
        const SUFFIX: &[u8] = br#""}"#;
        assert!(
            total_len >= PREFIX.len() + SUFFIX.len(),
            "body must fit the repaired envelope"
        );
        let mut body = Vec::with_capacity(total_len);
        body.extend_from_slice(PREFIX);
        body.extend_from_slice(&vec![b'x'; total_len - PREFIX.len() - SUFFIX.len()]);
        body.extend_from_slice(SUFFIX);
        body
    }

    #[test]
    fn repair_chunk_fold_keeps_capped_prefix() {
        // No stream involved: chunks summing past the cap must fail while
        // only the capped prefix is buffered, never after a full read.
        let mut buf = Vec::new();
        assert!(push_repair_chunk(
            &mut buf,
            b"hello",
            MAX_REPAIR_REPLY_BYTES
        ));
        assert!(push_repair_chunk(
            &mut buf,
            b" world!",
            MAX_REPAIR_REPLY_BYTES
        ));
        assert_eq!(buf, b"hello world!");
        assert!(!push_repair_chunk(
            &mut buf,
            &vec![b'x'; MAX_REPAIR_REPLY_BYTES],
            MAX_REPAIR_REPLY_BYTES
        ));
        // The failed push leaves the capped prefix intact.
        assert_eq!(buf, b"hello world!");
        // An exact fit is accepted; one byte more is not.
        let mut exact = Vec::new();
        assert!(push_repair_chunk(
            &mut exact,
            &vec![0u8; MAX_REPAIR_REPLY_BYTES],
            MAX_REPAIR_REPLY_BYTES
        ));
        assert_eq!(exact.len(), MAX_REPAIR_REPLY_BYTES);
        assert!(!push_repair_chunk(
            &mut exact,
            &[0u8; 1],
            MAX_REPAIR_REPLY_BYTES
        ));
        assert_eq!(exact.len(), MAX_REPAIR_REPLY_BYTES);
    }

    #[tokio::test]
    async fn repair_stream_pins_exact_ceiling_across_chunks() {
        use futures_util::stream::{self};
        let body = repaired_body_of_len(MAX_REPAIR_REPLY_BYTES);
        assert_eq!(body.len(), 65_536);
        // Split across chunks so the boundary is exercised by the drain,
        // not by a single whole-body buffer.
        let mid = body.len() / 2;
        let chunks = vec![
            Ok::<Vec<u8>, ()>(body[..mid].to_vec()),
            Ok(body[mid..].to_vec()),
        ];
        let drained = drain_repair_stream(stream::iter(chunks), MAX_REPAIR_REPLY_BYTES)
            .await
            .expect("exact ceiling across chunks must drain");
        assert_eq!(drained.len(), MAX_REPAIR_REPLY_BYTES);
        assert_eq!(
            classify_repair_reply(200, &drained),
            RepairCall::Repaired,
            "a well-formed repaired reply at exactly 65,536 bytes must stay Repaired"
        );
    }

    #[tokio::test]
    async fn repair_stream_rejects_ceiling_plus_one_from_length() {
        use futures_util::stream::{self};
        let body = repaired_body_of_len(MAX_REPAIR_REPLY_BYTES + 1);
        assert_eq!(body.len(), 65_537);
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("body must stay well-formed JSON");
        assert_eq!(
            value.get("status").and_then(|s| s.as_str()),
            Some("repaired"),
            "body must keep status=repaired so only the byte ceiling flips the verdict"
        );
        let mid = body.len() / 2;
        let chunks = vec![
            Ok::<Vec<u8>, ()>(body[..mid].to_vec()),
            Ok(body[mid..].to_vec()),
        ];
        // The drain itself must refuse the 65,537th byte: asserting only the
        // classifier afterwards would stay green even if the loop cap were
        // raised, because the classifier also rejects over-ceiling bodies.
        assert_eq!(
            drain_repair_stream(stream::iter(chunks), MAX_REPAIR_REPLY_BYTES).await,
            Err(RepairCall::Retry),
            "the 65,537th byte must fail closed from its length without retaining it"
        );
    }

    #[tokio::test]
    async fn repair_stream_chunk_error_fails_closed() {
        use futures_util::stream::{self};
        let chunks = vec![
            Ok::<Vec<u8>, ()>(br#"{"status":"repaired"}"#.to_vec()),
            Err(()),
        ];
        // A stream that yields a complete chunk and then errors must not fall
        // through to classify the partial body as Repaired.
        assert_eq!(
            drain_repair_stream(stream::iter(chunks), MAX_REPAIR_REPLY_BYTES).await,
            Err(RepairCall::Retry),
            "a chunk error after a complete prefix must fail closed to Retry"
        );
    }

    #[test]
    fn repair_request_serializes_for_the_container_post() {
        let req = decide_repair(JOB, USER, KEY, "par2-set-01", 1, 1)
            .unwrap()
            .unwrap();
        let body = serde_json::to_value(&req).unwrap();
        assert_eq!(body["par2_set_id"], "par2-set-01");
        assert_eq!(body["round"], 1);
        assert_eq!(
            body["scratch_prefix"],
            "ingest-scratch/job_0123456789abcdef/r1/"
        );
        assert_eq!(REPAIR_ENDPOINT, "/repair");
        assert_eq!(REPAIR_SERVICE_BINDING, "REPAIR_CONTAINER");
    }
}
