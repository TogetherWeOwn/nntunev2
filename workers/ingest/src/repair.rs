//! Repair dispatch: `Verifying` hands off to the Container batch job.
//!
//! par2 verify itself runs serverless; only when blocks are actually missing
//! does the job leave the worker for native-par2 repair in the Container.
//! The call goes through the service binding `REPAIR_CONTAINER` to the
//! no-route repair Worker (`workers/repair`), which streams the staged
//! scratch objects into its Container and streams the outputs back. No
//! par2 set + missing blocks means the job is unrepairable: the caller
//! fails it instead of dispatching.

use worker::{wasm_bindgen::JsValue, Env, Headers, Method, Request, RequestInit};

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
/// The result body is a small status document; anything larger is not
/// a repair reply we can trust.
const MAX_REPAIR_REPLY_BYTES: usize = 64 << 10;

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
        400 | 404 | 409 | 413 => RepairCall::Unrepairable,
        _ => RepairCall::Retry,
    }
}

/// Dispatch one repair request through the service binding. Transport
/// or binding failures are `Retry`, never a guessed terminal outcome.
pub async fn call_repair(env: &Env, req: &RepairRequest) -> RepairCall {
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
    // With the `http` feature the fetcher answers `http::Response`;
    // convert back to the classic response for a bounded body read.
    let mut response = match fetcher.fetch_request(request).await {
        Ok(response) => match worker::Response::try_from(response) {
            Ok(response) => response,
            Err(_) => return RepairCall::Retry,
        },
        Err(error) => {
            worker::console_log!("repair call failed job={}: {:?}", req.job_id, error);
            return RepairCall::Retry;
        }
    };
    let status = response.status_code();
    match response.bytes().await {
        Ok(bytes) => classify_repair_reply(status, &bytes),
        Err(_) => RepairCall::Retry,
    }
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
